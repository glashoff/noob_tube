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
//! The message carries no surface normal. Every client holds the same [`Level`] the server
//! does, built from the same numbers, so the normal a bullet hole needs is a raycast away and does
//! not need to be paid for on the wire.

use bevy::light::NotShadowCaster;
use avian3d::prelude::{Collider, Position, Rotation};
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::{MessageReceiver, Predicted, Rollback, client};
use noob_tube_shared::physics::Level;
use noob_tube_shared::hitbox::Hitbox;
use noob_tube_shared::player::{Player, PlayerInput, PlayerState};
use noob_tube_shared::shooting::{self, ShotFired};
use noob_tube_shared::simulation;
use noob_tube_shared::vehicle::Driven;

use crate::crosshair;
use crate::vehicle::MountedGun;

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
/// How far short of the shot's endpoint the surface under a bullet hole is looked for, and how far
/// the answer may be out before it stops being an answer about that endpoint at all.
///
/// See [`spawn_hole`]: the question is what is *at* the point the server reported, not what the
/// flight passed through on the way there.
const HOLE_PROBE: f32 = 0.15;
const HOLE_PROBE_SLACK: f32 = 0.07;

/// Where the local player's tracer starts *on foot*, relative to the camera: right, down, forward.
///
/// From the eye exactly, one's own tracer is a line seen end-on — a dot, or nothing. Real weapons
/// are held to one side of the head, and this is that offset and nothing more. A driver needs none
/// of it: they have a gun on the back with a barrel that ends somewhere real. See [`muzzle_of`].
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
/// A target that carries its own shape rather than deriving one, and whose seat may be taken.
type Solid = (
    Entity,
    &'static Collider,
    &'static Position,
    Option<&'static Rotation>,
    Option<&'static Driven>,
);

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
    mine: Option<Single<(Shooter, &Player), With<Predicted>>>,
    // Everyone else, at the position they are being *drawn* at. That is the honest local answer to
    // where the shot goes, and it is the same view the server reconstructs to resolve the hit.
    others: Query<Target, Drawn>,
    // Props are targets too, and they carry their hitbox rather than deriving one. `Driven` comes
    // with them so this client can leave out the vehicle it is sitting in, exactly as the server
    // does — the two must agree about what is not a target, or the tracer stops against a bonnet
    // the server shot straight through.
    props: Query<Solid, Drawn>,
    // Where our own tracer comes out, when we are driving something that has a gun on it.
    muzzles: Muzzles,
    level: Level,
    assets: Option<Res<ShotAssets>>,
    mut commands: Commands,
) {
    let (Some(mine), Some(assets)) = (mine, assets) else {
        return;
    };
    let ((action, state), me) = *mine;
    let targets = others
        .iter()
        .map(|(entity, other)| (entity, Hitbox::of(other)))
        .chain(
            props
                .iter()
                .filter(|(.., driven)| driven.is_none_or(|driven| driven.0 != me.peer))
                .map(|(entity, collider, position, rotation, _)| {
                    // `Rotation` arrives separately from `Position` and may not have landed yet —
                    // lightyear inserts an interpolated component only after two updates. Facing
                    // forward until it does is right for a crate and harmless for anything else:
                    // the pose is corrected on the next update, and the server's answer is the one
                    // that counts either way.
                    let facing = rotation.map_or(Quat::IDENTITY, |rotation| rotation.0);
                    (entity, Hitbox::new(collider.clone(), position.0, facing))
                }),
        );
    // The same call the server makes, over the same input, before either side steps the player.
    let Some(fired) = shooting::fire(&level, state, &action.0, targets) else {
        return;
    };
    let end = fired.point();
    let start = muzzles
        .of(me.peer)
        .unwrap_or_else(|| beside_the_eye(fired.origin, end));
    spawn_tracer(&mut commands, &assets, start, end);
}

/// Update: draws every shot the server has told us about.
fn draw_shots(
    mut inbox: Query<&mut MessageReceiver<ShotFired>>,
    muzzles: Muzzles,
    // What a bullet hole is put on, and hung from so that it moves when that thing does.
    mut decals: Decals,
    mine: Option<Single<&Player, With<Predicted>>>,
    assets: Res<ShotAssets>,
    mut commands: Commands,
) {
    let own_peer = mine.map(|player| player.peer);
    for mut receiver in inbox.iter_mut() {
        for shot in receiver.receive() {
            let own = own_peer == Some(shot.shooter);
            // Our own tracer was drawn the moment we fired — see `predict_own_tracer`. Drawing it
            // again now would put a second line half a round trip behind the first.
            if !own {
                // From their gun if they are behind one, and from their eye if they are not —
                // which is the point the server cast the shot from either way.
                let start = muzzles.of(shot.shooter).unwrap_or(shot.from);
                spawn_tracer(&mut commands, &assets, start, shot.to);
            }

            if shot.hit_player {
                // No decal on a player: they move, and a hole pinned to the world where they
                // stood is a mark on nothing. What the shooter gets instead is the crosshair.
                if own {
                    commands.trigger(crosshair::HitLanded);
                }
                continue;
            }
            spawn_hole(&mut commands, &assets, &mut decals, &shot);
        }
    }
}

/// The mounted guns on the map, and who is behind each of them.
#[derive(SystemParam)]
struct Muzzles<'w, 's> {
    guns: Query<'w, 's, (&'static MountedGun, &'static GlobalTransform)>,
    seats: Query<'w, 's, &'static Driven>,
}

impl Muzzles<'_, '_> {
    /// Where `peer`'s tracer comes out, in world space.
    ///
    /// `None` for anyone on foot, for the driver of a vehicle with no gun on it, and on a client
    /// that has no model to hang one from — all three of which fall back to the eye, which is where
    /// the shot was cast from and so is never wrong, only less interesting.
    ///
    /// Asked of each gun rather than by looking a vehicle up from the peer, because a gun knows its
    /// own chassis and there are at most a handful of them.
    fn of(&self, peer: u64) -> Option<Vec3> {
        self.guns
            .iter()
            .find(|(gun, _)| self.seats.get(gun.chassis).is_ok_and(|seat| seat.0 == peer))
            .map(|(gun, placed)| placed.transform_point(gun.muzzle))
    }
}

/// What a bullet hole is put on: the world it has to be found in, the things it can be hung from,
/// and the tally that keeps a firefight from becoming an unbounded pile of quads.
#[derive(SystemParam)]
struct Decals<'w, 's> {
    level: Level<'w, 's>,
    // Everything a decal can be hung on. A crate and a vehicle are in here; the level's collision
    // geometry is not, because it carries no `Visibility` for a child to inherit — and it has no
    // need to, being the one thing that never moves.
    surfaces: Query<'w, 's, &'static GlobalTransform, With<Visibility>>,
    holes: ResMut<'w, Holes>,
}

/// Where one's own tracer starts when there is no gun to start it from: beside the eye.
fn beside_the_eye(from: Vec3, to: Vec3) -> Vec3 {
    let direction = (to - from).normalize_or_zero();
    // Right of the eye and a little below it, so one's own tracer is a line rather than a dot.
    let right = direction.cross(Vec3::Y).normalize_or_zero();
    // Never more than a third of the way to the target: at point-blank range a muzzle a fixed
    // 1.6 m out puts most of the tracer against the camera, where perspective makes it a blob.
    let ahead = MUZZLE_OFFSET.z.min(from.distance(to) * 0.35);
    from + right * MUZZLE_OFFSET.x + Vec3::Y * MUZZLE_OFFSET.y + direction * ahead
}

/// The bright line along the shot's path, from wherever it is drawn as coming out of.
fn spawn_tracer(commands: &mut Commands, assets: &ShotAssets, start: Vec3, to: Vec3) {
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
/// The normal comes from casting a ray locally. If that finds nothing — the shot ended in mid-air
/// at the weapon's range, or the level here differs from the server's — there is no surface to put
/// a mark on, and none is drawn.
///
/// That ray is cast over the last hand's breadth before the endpoint rather than along the whole
/// flight, and the difference matters. The server's shot ignores the vehicle its shooter is sitting
/// in; a ray from the eye does not, and it stops against that vehicle's own bodywork — which put
/// the hole in the right place on the ground and then hung it on the buggy, so it drove off with
/// it. The neighbourhood of the point the server reported is the question that was actually meant,
/// and nothing the shot passed through can answer it.
fn spawn_hole(commands: &mut Commands, assets: &ShotAssets, decals: &mut Decals, shot: &ShotFired) {
    let direction = (shot.to - shot.from).normalize_or_zero();
    let probe = shot.to - direction * HOLE_PROBE;
    let Some((distance, normal, surface)) =
        decals.level.surface_hit(probe, direction, HOLE_PROBE * 2.0)
    else {
        return;
    };
    // And what it found has to be the endpoint itself. It is not when the probe began inside
    // something — a shot into the ground close beside a vehicle can start within its bodywork —
    // and then there is no surface here to speak of and nothing honest to hang a mark on.
    if (distance - HOLE_PROBE).abs() > HOLE_PROBE_SLACK {
        return;
    }
    // The normal can point either way along the surface; a decal wants the side it was shot from.
    let facing = if normal.dot(direction) > 0.0 { -normal } else { normal };
    // A hole on the floor faces straight up, which is exactly where `looking_to` cannot use Y as
    // its up vector — the two would be parallel and the rotation undefined.
    let up = if facing.dot(Vec3::Y).abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    // Turned by a different amount each time, so a wall taking fire does not become a grid of
    // identical squares. Derived from the point itself, so it is stable rather than random: every
    // client draws the same hole the same way round.
    let spin = Quat::from_rotation_z(shot.to.x * 7.3 + shot.to.y * 3.1 + shot.to.z * 5.7);
    // A `Rectangle` faces +Z, and `looking_to` points -Z, so it is aimed into the wall to lay the
    // front of the quad flat against it.
    let pose = Transform::from_translation(shot.to + facing * HOLE_LIFT)
        .looking_to(-facing, up)
        .with_rotation(Transform::default().looking_to(-facing, up).rotation * spin);

    let mut hole = commands.spawn((
        Name::from("Bullet hole"),
        Ephemeral(HOLE_SECONDS),
        // It lies a centimetre off the wall; its shadow would land on the wall beside it.
        NotShadowCaster,
        Mesh3d(assets.hole.clone()),
        MeshMaterial3d(assets.hole_material.clone()),
        pose,
    ));
    // Hung on what it hit, when that is something that can move. A hole pinned in world space on a
    // crate somebody then shoves is a mark hanging in the air where the crate used to be — the same
    // objection that keeps decals off players, only slower and so easier to miss.
    if let Ok(host) = decals.surfaces.get(surface) {
        hole.insert((GlobalTransform::from(pose).reparented_to(host), ChildOf(surface)));
    }
    let hole = hole.id();
    decals.holes.0.push_back(hole);
    // The oldest goes when there are too many. It may already have timed out and been despawned,
    // which `try_despawn` is fine with.
    while decals.holes.0.len() > MAX_HOLES {
        if let Some(oldest) = decals.holes.0.pop_front() {
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
