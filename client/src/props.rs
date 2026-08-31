//! Draws the things in the world that move but are nobody's player.
//!
//! Nothing here simulates anything. The crates arrive from the server carrying [`Hitbox`], which is
//! both their shape and their place, and lightyear interpolates it between received updates exactly
//! as it does a player's position. The client never works out where a crate ought to be — see
//! [`props`](noob_tube_shared::props) for why that is deliberate even though it could.
//!
//! Because the hitbox *is* what is drawn, the box on screen and the box a shot is tested against
//! cannot drift apart. For players the two are separate — a capsule collides, a body and a head are
//! drawn — and keeping them in step needed a test.

use bevy::prelude::*;
use lightyear::prelude::{InterpolationSystems, client};
use noob_tube_shared::hitbox::Hitbox;

/// A prop that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<Hitbox>);

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (give_bodies, place_bodies)
                .chain()
                // Interpolation writes the smoothed `Hitbox` in Update too. Without this the crate
                // would render last frame's sample, one frame behind everything else.
                .after(InterpolationSystems::All),
        );
    }
}

/// Update: gives an arrived prop something to be seen as.
///
/// One mesh per prop rather than a shared one, because each is sized from its own hitbox. There are
/// two of them; sharing by size belongs with the asset handling in M2.
fn give_bodies(
    arrived: Query<(Entity, &Hitbox), Arrived>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (entity, hitbox) in arrived.iter() {
        let Hitbox::Prop { half_extents, .. } = *hitbox else {
            continue;
        };
        commands.entity(entity).insert((
            Name::from("Moving crate"),
            Mesh3d(meshes.add(Cuboid::from_size(half_extents * 2.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.35, 0.45, 0.7))),
            Transform::default(),
        ));
        info!("drawing a moving crate");
    }
}

/// Update: follows the interpolated hitbox.
fn place_bodies(mut props: Query<(&Hitbox, &mut Transform), With<client::Remote>>) {
    for (hitbox, mut transform) in props.iter_mut() {
        if let Hitbox::Prop { centre, .. } = *hitbox {
            transform.translation = centre;
        }
    }
}
