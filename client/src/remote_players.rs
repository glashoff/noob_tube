//! Draws the players the server replicates to us.
//!
//! This is the receiving half of replication. Nothing here spawns a player: the entities arrive
//! from the server carrying [`Player`], [`PlayerState`] and [`Aim`], and this module only gives them
//! something visible. A capsule, because it is the exact shape the movement code collides with —
//! character models are M2.

use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::movement::{CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET};
use noob_tube_shared::player::{Aim, Player, PlayerState};

pub struct RemotePlayersPlugin;

impl Plugin for RemotePlayersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (give_bodies, place_bodies).chain());
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
