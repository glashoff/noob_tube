//! Draws the players the server replicates to us.
//!
//! This is the receiving half of replication. Nothing here spawns a player: the entities arrive
//! from the server carrying [`Player`], [`PlayerState`] and [`Aim`], and this module only gives them
//! something visible. A capsule, because it is the exact shape the movement code collides with —
//! character models are M2.
//!
//! The values are already smoothed by the time anything here reads them. The server marks every
//! player `Interpolated` for every client but its owner, and lightyear then keeps a history of
//! received updates and writes a blend of two of them back into the component each frame. That
//! happens in place, on the same entity — there is no second copy to look up.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use noob_tube_shared::movement::{
    BODY_HALF_HEIGHT, BODY_RADIUS, BODY_Y_OFFSET, HEAD_SIZE, HEAD_Y,
};
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState};
use noob_tube_shared::vehicle::Driving;
use noob_tube_shared::tuning::NetConfig;

use crate::local_player::CurrentInput;

pub struct RemotePlayersPlugin;

impl Plugin for RemotePlayersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (give_bodies, claim_own_player, hide_the_seated, place_bodies, place_heads)
                .chain()
                // Interpolation writes the smoothed values into `PlayerState` and `Aim` in Update
                // as well. Without this the capsules would render whatever last frame's sample
                // was — one frame of lag added on top of the delay interpolation already costs.
                .after(InterpolationSystems::All),
        )
            // Inputs must be written before lightyear packs them for sending, which is what this
            // system set marks.
            .add_systems(Update, report_input_delay)
            .add_systems(
                FixedPreUpdate,
                send_input
                    .in_set(client::input::InputSystems::WriteClientInputs)
                    // A rollback re-runs this schedule for each replayed tick, and the input for
                    // those ticks has to come from the buffer, not from whatever key is held now.
                    .run_if(not(resource_exists::<Rollback>)),
            );
    }
}

/// Update: reports the input delay lightyear settled on, once, when the clocks agree.
///
/// The configured value is a floor and a ceiling, not the answer: between them lightyear picks a
/// delay from the measured round trip, so what is actually in effect is only knowable at runtime.
/// Printing it also closes the gap between "the setting was read" and "the setting is doing
/// something", which is not the same thing and has twice not been today.
fn report_input_delay(
    timeline: Res<client::LocalTimelineSync>,
    net: Res<NetConfig>,
    mut reported: Local<bool>,
) {
    if *reported || !timeline.is_synced() {
        return;
    }
    *reported = true;
    let ticks = timeline.input_delay();
    info!(
        "clocks synced: input delay {ticks} ticks ({:?})",
        net.tick_duration() * u32::from(ticks)
    );
}

/// A player's head, a child of the capsule. Carries the pitch the capsule cannot.
#[derive(Component)]
struct PlayerHead;

/// Update: gives a replicated player a capsule to be seen as.
///
/// `Without<Predicted>` is what leaves our own player out. Our entity arrives over the network like
/// everyone else's, but the camera sits inside it, so a capsule there would fill the screen. It is
/// also the entity the mesh would be least honest about: it is the one being corrected.
///
/// The mesh and material are created per player, which is wasteful and fine for a handful; sharing
/// them belongs with the asset handling in M2.
fn give_bodies(
    arrived: Query<
        (Entity, &Player),
        (With<client::Remote>, Without<Predicted>, Added<PlayerState>),
    >,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    let head_mesh = meshes.add(Cuboid::from_length(HEAD_SIZE));
    let head_material = materials.add(Color::srgb(0.9, 0.75, 0.6));
    // A nose, so the direction is unmistakable. A cube alone looks the same from four sides.
    let nose_mesh = meshes.add(Cuboid::new(0.08, 0.08, 0.22));
    let nose_material = materials.add(Color::srgb(0.15, 0.15, 0.18));

    for (entity, player) in arrived.iter() {
        commands
            .entity(entity)
            .insert((
                Name::from(format!("Remote player {}", player.peer)),
                Mesh3d(meshes.add(Capsule3d::new(BODY_RADIUS, BODY_HALF_HEIGHT * 2.0))),
                MeshMaterial3d(materials.add(Color::srgb(0.8, 0.3, 0.25))),
                Transform::default(),
            ))
            .with_children(|body| {
                body.spawn((
                    Name::from("Head"),
                    PlayerHead,
                    Mesh3d(head_mesh.clone()),
                    MeshMaterial3d(head_material.clone()),
                    // Both the body and this are sized so the whole silhouette fits inside the
                    // collision capsule, which is the hitbox — see `movement.rs`, where a test
                    // holds that to account. The first version of this head sat from 1.70 m to
                    // 2.04 m, entirely above the 1.70 m capsule: perfectly visible and impossible
                    // to shoot.
                    //
                    // The transform is relative to the body capsule's centre, not the feet.
                    Transform::from_xyz(0.0, HEAD_Y - BODY_Y_OFFSET, 0.0),
                ))
                .with_children(|head| {
                    // Forward is -Z, matching the convention movement uses.
                    head.spawn((
                        Name::from("Nose"),
                        Mesh3d(nose_mesh.clone()),
                        MeshMaterial3d(nose_material.clone()),
                        Transform::from_xyz(0.0, 0.0, -HEAD_SIZE * 0.8),
                    ));
                });
            });
        info!("drawing player {}", player.peer);
    }
}

/// Update: follows the replicated state with the capsule.
///
/// `PlayerState::position` is the feet, the capsule mesh is centred, hence the offset. Yaw is
/// applied but pitch is not: a capsule leaning back would look wrong, and a real body only turns at
/// the waist. That is M2's problem.
///
/// Our own player is excluded for the same reason it gets no mesh: the camera is inside it.
fn place_bodies(
    mut bodies: Query<(&PlayerState, &Aim, &mut Transform), (With<client::Remote>, Without<Predicted>)>,
) {
    for (state, aim, mut transform) in bodies.iter_mut() {
        transform.translation = state.position + Vec3::Y * BODY_Y_OFFSET;
        transform.rotation = Quat::from_rotation_y(aim.yaw);
    }
}

/// Update: a player in a vehicle is inside the bodywork, so stop drawing them.
///
/// Hidden rather than despawned. They are still a replicated entity with a pose and a hitbox — they
/// can still be shot, and they come back the moment they get out — and rebuilding the meshes on
/// every exit would be work done for nothing.
///
/// Cheap enough to run every frame: it writes only when the answer changes, so the change detection
/// downstream stays quiet.
fn hide_the_seated(mut bodies: Query<(&mut Visibility, Has<Driving>), With<PlayerState>>) {
    for (mut visibility, driving) in bodies.iter_mut() {
        let wanted = if driving { Visibility::Hidden } else { Visibility::Inherited };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Update: takes ownership of the player the server says is ours.
///
/// `Controlled` arrives on exactly one replicated entity: the one the server marked `ControlledBy`
/// our connection. That is a better answer than comparing peer ids, because the server decides it
/// and the client cannot get it wrong.
///
/// `InputMarker` tells lightyear which `ActionState` this client fills in, as opposed to the ones it
/// merely receives for other players.
///
/// It used to copy the server's spawn position into a separate local simulation as well. There is
/// no separate simulation any more — this entity is the simulation — so it starts wherever the
/// server put it, and there is nothing to adopt.
fn claim_own_player(
    mine: Query<(Entity, &Player), (With<client::Remote>, Added<Controlled>)>,
    mut commands: Commands,
) {
    for (entity, player) in mine.iter() {
        commands
            .entity(entity)
            .insert(InputMarker::<PlayerInput>::default());
        info!("player {} is ours", player.peer);
    }
}

/// FixedPreUpdate: hands this tick's input to lightyear.
///
/// Writing it into `ActionState` is the whole of sending: the plugin buffers it, packs the last N
/// ticks into the next packet, and keeps the history that M4's rollback will replay from.
///
/// This is also the input the local prediction steps on, in the same tick: the client does not wait
/// for the server to tell it what its own input did.
fn send_input(
    input: Res<CurrentInput>,
    mut action: Single<&mut ActionState<PlayerInput>, With<InputMarker<PlayerInput>>>,
) {
    action.0 = input.0;
}

/// Update: pitches each head to match its player's aim.
///
/// Yaw already turns the whole body, so the head only carries pitch. Splitting them this way is
/// what a real character does too — the legs face where you walk, the head looks where you aim —
/// and it is the reason `Aim` is replicated at all.
fn place_heads(
    mut heads: Query<(&ChildOf, &mut Transform), With<PlayerHead>>,
    bodies: Query<&Aim>,
) {
    for (parent, mut transform) in heads.iter_mut() {
        let Ok(aim) = bodies.get(parent.parent()) else {
            continue;
        };
        transform.rotation = Quat::from_rotation_x(aim.pitch);
    }
}
