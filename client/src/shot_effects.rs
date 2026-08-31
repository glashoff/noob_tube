//! What a shot looks like.
//!
//! The server resolves every shot and tells everyone about it — see
//! [`ShotFired`](noob_tube_shared::shooting::ShotFired). Nothing here decides anything: a client
//! that drew its own shots would show hits the server never granted, and two players would be
//! looking at different fights.
//!
//! Two things get drawn. A **tracer**, a bright line along the path, gone in a twentieth of a
//! second — which is what makes fire directional, so being shot at from somewhere is different
//! from being shot at. And a **bullet hole** where it struck the level, which is what makes a fight
//! leave a mark. A shot that struck a player leaves neither hole nor blood yet; it puts a marker on
//! the shooter's crosshair instead, which is the feedback that actually matters.
//!
//! The message carries no surface normal. Every client holds the same [`CollisionWorld`] the server
//! does, built from the same numbers, so the normal a bullet hole needs is a raycast away and does
//! not need to be paid for on the wire.

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::{MessageReceiver, Predicted, Rollback, client};
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::hitbox::Hitbox;
use noob_tube_shared::player::{Player, PlayerInput, PlayerState};
use noob_tube_shared::shooting::{self, ShotFired};
use noob_tube_shared::simulation;

use crate::crosshair;

/// How long a tracer stays up. Long enough to see, short enough that a burst reads as several
/// shots rather than one bar of light.
const TRACER_SECONDS: f32 = 0.05;
/// How thick a tracer is, in metres. Thin: near the camera, perspective makes a tracer of any real
/// thickness into a wedge across half the screen.
const TRACER_WIDTH: f32 = 0.012;
/// How long a bullet hole stays before it is forgotten.
const HOLE_SECONDS: f32 = 12.0;
/// How many bullet holes exist at once. Past this the oldest goes, so a long firefight cannot turn
/// into an unbounded pile of entities.
const MAX_HOLES: usize = 160;
/// How wide a bullet hole is.
const HOLE_SIZE: f32 = 0.09;
/// How far a hole floats off the surface, so it does not fight the wall for the same pixels.
const HOLE_LIFT: f32 = 0.01;

/// Where the local player's tracer starts, relative to the camera: right, down, and forward.
///
/// From the eye exactly, one's own tracer is a line seen end-on — a dot, or nothing. Real weapons
/// are held to one side of the head, and this is that offset and nothing more.
const MUZZLE_OFFSET: Vec3 = Vec3::new(0.14, -0.12, 1.6);

pub struct ShotEffectsPlugin;

impl Plugin for ShotEffectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Holes>()
            .add_systems(Startup, load_assets)
            .add_systems(Update, (draw_shots, forget_effects))
            .add_systems(
                FixedUpdate,
                predict_own_tracer
                    // Before the step, exactly as on the server: the step starts the cooldown, so
                    // afterwards the answer to "is this tick a shot" is always no. Same rule, same
                    // order, same answer.
                    .before(simulation::step_players::<With<Predicted>>)
                    // A rollback re-runs this schedule for every replayed tick. Without this a
                    // single correction would redraw every tracer of the last twenty ticks at once.
                    .run_if(not(resource_exists::<Rollback>)),
            );
    }
}

/// The meshes and materials every effect shares.
///
/// Built once. A tracer per shot at eight rounds a second, each allocating its own mesh, is how a
/// frame budget disappears into asset churn.
#[derive(Resource)]
struct ShotAssets {
    tracer: Handle<Mesh>,
    tracer_material: Handle<StandardMaterial>,
    hole: Handle<Mesh>,
    hole_material: Handle<StandardMaterial>,
}

/// Something that draws for a moment and then is gone. The value is seconds left.
#[derive(Component)]
struct Ephemeral(f32);

/// Every bullet hole on the level, oldest first.
///
/// Holes outlive everything else here by two orders of magnitude, so they are the one effect that
/// can pile up. This is what caps them; entries for holes that have already timed out are dropped
/// when they are next looked at, so nothing has to be kept in step.
#[derive(Resource, Default)]
struct Holes(std::collections::VecDeque<Entity>);

/// Startup: builds the shared meshes and materials.
fn load_assets(
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    commands.insert_resource(ShotAssets {
        // A unit-long box along -Z, so a tracer is this scaled in Z by its length and pointed with
        // `looking_at` — the same convention the aim ray uses.
        tracer: meshes.add(Cuboid::new(TRACER_WIDTH, TRACER_WIDTH, 1.0)),
        tracer_material: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.85, 0.4),
            // Unlit: a tracer is its own light source, and one that dimmed in shadow would vanish
            // exactly where it matters most.
            unlit: true,
            ..default()
        }),
        hole: meshes.add(Rectangle::new(HOLE_SIZE, HOLE_SIZE)),
        hole_material: materials.add(StandardMaterial {
            base_color: Color::srgb(0.06, 0.05, 0.05),
            perceptual_roughness: 1.0,
            // Both faces, so a hole is still there when seen from the other side of a thin prop.
            cull_mode: None,
            ..default()
        }),
    });
}

/// What a shot is fired from: the trigger and this tick's input, plus where the eye is.
type Shooter = (&'static ActionState<PlayerInput>, &'static PlayerState);
/// A player a shot could stop in.
type Target = (Entity, &'static PlayerState);
/// Everyone the server is replicating to us except ourselves.
type Drawn = (With<client::Remote>, Without<Predicted>);

/// FixedUpdate: draws our own tracer the moment we fire, without waiting to be told.
///
/// The client is not deciding anything here. `fire_cooldown` lives in `PlayerState`, which is
/// predicted, so the client and the server run the same rule over the same input and pick out the
/// same ticks; this only *uses* an answer the client already had. The server remains the authority,
/// and a client that lied about its rate of fire would still be ignored — the shot it invented has
/// no effect on anyone.
///
/// What it buys is the same thing predicting the cooldown buys: at 100 ms of ping, waiting for the
/// server's `ShotFired` puts half a round trip between the click and the line on screen, on top of
/// a tracer that only lives for 50 ms. That reads as a weapon disconnected from the trigger.
///
/// Only the tracer. The bullet hole and the hit marker still come from the server, and they can
/// afford to: a hole lasts twelve seconds, so arriving 50 ms late is invisible, and a *hit* is
/// exactly the thing a client must never guess at — the server rewinds the world to decide it, and
/// a predicted kill it then denied could not be taken back.
fn predict_own_tracer(
    mine: Option<Single<Shooter, With<Predicted>>>,
    // Everyone else, at the position they are being *drawn* at. That is the honest local answer to
    // where the shot goes, and it is the same view the server reconstructs to resolve the hit.
    others: Query<Target, Drawn>,
    // Props are targets too, and they carry their hitbox rather than deriving one.
    props: Query<(Entity, &Hitbox), Drawn>,
    world: Option<Res<CollisionWorld>>,
    assets: Option<Res<ShotAssets>>,
    mut commands: Commands,
) {
    let (Some(mine), Some(world), Some(assets)) = (mine, world, assets) else {
        return;
    };
    let (action, state) = *mine;
    let targets = others
        .iter()
        .map(|(entity, other)| (entity, Hitbox::of(other)))
        .chain(props.iter().map(|(entity, hitbox)| (entity, *hitbox)));
    // The same call the server makes, over the same input, before either side steps the player.
    let Some(fired) = shooting::fire(&world, state, &action.0, targets) else {
        return;
    };
    spawn_tracer(&mut commands, &assets, fired.origin, fired.point(), true);
}

/// Update: draws every shot the server has told us about.
fn draw_shots(
    mut inbox: Query<&mut MessageReceiver<ShotFired>>,
    world: Option<Res<CollisionWorld>>,
    mine: Option<Single<&Player, With<Predicted>>>,
    assets: Res<ShotAssets>,
    mut holes: ResMut<Holes>,
    mut commands: Commands,
) {
    let own_peer = mine.map(|player| player.peer);
    for mut receiver in inbox.iter_mut() {
        for shot in receiver.receive() {
            let own = own_peer == Some(shot.shooter);
            // Our own tracer was drawn the moment we fired — see `predict_own_tracer`. Drawing it
            // again now would put a second line half a round trip behind the first.
            if !own {
                spawn_tracer(&mut commands, &assets, shot.from, shot.to, false);
            }

            if shot.hit_player {
                // No decal on a player: they move, and a hole pinned to the world where they
                // stood is a mark on nothing. What the shooter gets instead is the crosshair.
                if own {
                    commands.trigger(crosshair::HitLanded);
                }
                continue;
            }
            if let Some(world) = world.as_deref() {
                spawn_hole(&mut commands, &assets, world, &shot, &mut holes);
            }
        }
    }
}

/// The bright line along the shot's path.
fn spawn_tracer(commands: &mut Commands, assets: &ShotAssets, from: Vec3, to: Vec3, own: bool) {
    let direction = (to - from).normalize_or_zero();
    if direction == Vec3::ZERO {
        return;
    }
    let start = if own {
        // Right of the eye and a little below it, so one's own tracer is a line rather than a dot.
        let right = direction.cross(Vec3::Y).normalize_or_zero();
        // Never more than a third of the way to the target: at point-blank range a muzzle a fixed
        // 1.6 m out puts most of the tracer against the camera, where perspective makes it a blob.
        let ahead = MUZZLE_OFFSET.z.min(from.distance(to) * 0.35);
        from + right * MUZZLE_OFFSET.x + Vec3::Y * MUZZLE_OFFSET.y + direction * ahead
    } else {
        from
    };
    let length = start.distance(to);
    if length < 0.01 {
        return;
    }
    commands.spawn((
        Name::from("Tracer"),
        Ephemeral(TRACER_SECONDS),
        // A tracer is light, not an object. Left casting shadows it draws a black stripe across
        // the ground beside every shot, which is the single most obviously wrong thing on screen.
        NotShadowCaster,
        Mesh3d(assets.tracer.clone()),
        MeshMaterial3d(assets.tracer_material.clone()),
        Transform::from_translation(start.lerp(to, 0.5))
            .looking_at(to, Vec3::Y)
            // The mesh is a unit box along Z; stretching it is what makes it a line.
            .with_scale(Vec3::new(1.0, 1.0, length)),
    ));
}

/// The mark left where a shot met the level.
///
/// The normal comes from casting the same ray locally. If that finds nothing — the shot ended in
/// mid-air at the weapon's range, or the level here differs from the server's — there is no surface
/// to put a mark on, and none is drawn.
fn spawn_hole(
    commands: &mut Commands,
    assets: &ShotAssets,
    world: &CollisionWorld,
    shot: &ShotFired,
    holes: &mut Holes,
) {
    let direction = (shot.to - shot.from).normalize_or_zero();
    let reach = shot.from.distance(shot.to) + 0.1;
    let Some((_, normal)) = world.raycast_normal(shot.from, direction, reach) else {
        return;
    };
    // The normal can point either way along the surface; a decal wants the side it was shot from.
    let facing = if normal.dot(direction) > 0.0 { -normal } else { normal };
    // A hole on the floor faces straight up, which is exactly where `looking_to` cannot use Y as
    // its up vector — the two would be parallel and the rotation undefined.
    let up = if facing.dot(Vec3::Y).abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    // Turned by a different amount each time, so a wall taking fire does not become a grid of
    // identical squares. Derived from the point itself, so it is stable rather than random: every
    // client draws the same hole the same way round.
    let spin = Quat::from_rotation_z(shot.to.x * 7.3 + shot.to.y * 3.1 + shot.to.z * 5.7);
    let hole = commands
        .spawn((
            Name::from("Bullet hole"),
            Ephemeral(HOLE_SECONDS),
            // It lies a centimetre off the wall; its shadow would land on the wall beside it.
            NotShadowCaster,
            Mesh3d(assets.hole.clone()),
            MeshMaterial3d(assets.hole_material.clone()),
            // A `Rectangle` faces +Z, and `looking_to` points -Z, so it is aimed into the wall to
            // lay the front of the quad flat against it.
            Transform::from_translation(shot.to + facing * HOLE_LIFT)
                .looking_to(-facing, up)
                .with_rotation(Transform::default().looking_to(-facing, up).rotation * spin),
        ))
        .id();
    holes.0.push_back(hole);
    // The oldest goes when there are too many. It may already have timed out and been despawned,
    // which `try_despawn` is fine with.
    while holes.0.len() > MAX_HOLES {
        if let Some(oldest) = holes.0.pop_front() {
            commands.entity(oldest).try_despawn();
        }
    }
}

/// Update: takes away everything whose moment has passed.
fn forget_effects(
    time: Res<Time>,
    mut effects: Query<(Entity, &mut Ephemeral)>,
    mut commands: Commands,
) {
    for (entity, mut effect) in effects.iter_mut() {
        effect.0 -= time.delta_secs();
        if effect.0 <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}
