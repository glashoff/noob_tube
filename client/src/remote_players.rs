//! Draws the players the server replicates to us.
//!
//! This is the receiving half of replication. Nothing here spawns a player: the entities arrive
//! from the server carrying [`Player`], [`PlayerState`] and [`Aim`], and this module only gives them
//! something visible. A capsule, because it is the exact shape the movement code collides with —
//! character models are M2.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use noob_tube_shared::movement::{CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET};
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState};

use crate::local_player::{CurrentInput, LocalPlayer};

pub struct RemotePlayersPlugin;

impl Plugin for RemotePlayersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (give_bodies, claim_own_player, place_bodies).chain())
            // Inputs must be written before lightyear packs them for sending, which is what this
            // system set marks.
            .add_systems(
                FixedPreUpdate,
                send_input.in_set(client::input::InputSystems::WriteClientInputs),
            );
    }
}

/// Update: gives a replicated player a capsule to be seen as.
///
/// `client::Remote` marks entities that arrived over the network rather than being spawned locally,
/// so this can never fire on our own [`LocalPlayer`](crate::local_player::LocalPlayer).
///
/// The mesh and material are created per player, which is wasteful and fine for a handful; sharing
/// them belongs with the asset handling in M2.
fn give_bodies(
    arrived: Query<(Entity, &Player), (With<client::Remote>, Added<PlayerState>)>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (entity, player) in arrived.iter() {
        commands.entity(entity).insert((
            Name::from(format!("Remote player {}", player.peer)),
            Mesh3d(meshes.add(Capsule3d::new(CAPSULE_RADIUS, CAPSULE_HALF_HEIGHT * 2.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.8, 0.3, 0.25))),
            Transform::default(),
        ));
        info!("drawing player {}", player.peer);
    }
}

/// Update: follows the replicated state with the capsule.
///
/// `PlayerState::position` is the feet, the capsule mesh is centred, hence the offset. Yaw is
/// applied but pitch is not: a capsule leaning back would look wrong, and a real body only turns at
/// the waist. That is M2's problem.
fn place_bodies(
    mut bodies: Query<(&PlayerState, &Aim, &mut Transform), With<client::Remote>>,
) {
    for (state, aim, mut transform) in bodies.iter_mut() {
        transform.translation = state.position + Vec3::Y * CAPSULE_Y_OFFSET;
        transform.rotation = Quat::from_rotation_y(aim.yaw);
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
fn claim_own_player(
    mine: Query<(Entity, &Player, &PlayerState), (With<client::Remote>, Added<Controlled>)>,
    mut local: Single<&mut LocalPlayer>,
    mut commands: Commands,
) {
    for (entity, player, state) in mine.iter() {
        commands
            .entity(entity)
            .insert(InputMarker::<PlayerInput>::default());
        // Adopt the server's state. The local simulation starts at the origin, but the server puts
        // the nth player at its own spawn point, so without this the second client's camera stands
        // two metres away from its own capsule — the first visible symptom of two simulations that
        // never agree on anything but by accident.
        //
        // This is a one-off correction at the moment ownership is established. Doing it on every
        // update is reconciliation, which needs a rollback to be worth anything, and that is M4.
        local.state = *state;
        info!("player {} is ours, adopting server state", player.peer);
    }
}

/// FixedPreUpdate: hands this tick's input to lightyear.
///
/// Writing it into `ActionState` is the whole of sending: the plugin buffers it, packs the last N
/// ticks into the next packet, and keeps the history that M4's rollback will replay from.
///
/// The local player keeps simulating itself in `local_player.rs` as well. For now that is two
/// simulations running side by side rather than prediction — reconciling them is M4.
fn send_input(
    input: Res<CurrentInput>,
    mut action: Single<&mut ActionState<PlayerInput>, With<InputMarker<PlayerInput>>>,
) {
    action.0 = input.0;
}
