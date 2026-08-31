//! Draws the things in the world that move but are nobody's player.
//!
//! Nothing here simulates anything, and nothing here places anything either. A crate arrives as a
//! [`Prop`] — its shape, sent once — and an Avian [`Position`], which lightyear interpolates between
//! received updates exactly as it does a player's. `LightyearAvianPlugin` writes that interpolated
//! pose into `Transform` in `PostUpdate`, so all this module does is give the crate something to be
//! seen as. See [`props`](noob_tube_shared::props) for why the motion is not computed locally even
//! though it could be.
//!
//! Because the mesh is built from the same [`Prop`] the hit test reads, the box on screen and the
//! box a shot is tested against cannot drift apart. For players the two are separate — a capsule
//! collides, a body and a head are drawn — and keeping them in step needed a test.

use avian3d::prelude::{ColliderDensity, RigidBody};
use bevy::prelude::*;
use lightyear::prelude::{Predicted, client};
use noob_tube_shared::props::Prop;

/// A prop that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<Prop>);

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, give_bodies);
    }
}

/// Update: gives an arrived prop something to be seen as, and something to be hit.
///
/// One mesh per prop rather than a shared one, because each is sized from its own shape. There are
/// two of them; sharing by size belongs with the asset handling in M2.
///
/// The collider is here so that a client asks the same question of a prop that the server does —
/// `Hitbox::new(collider, pose)` on both sides — rather than rebuilding a shape from the size and
/// hoping the two agree. It carries no [`RigidBody`](avian3d::prelude::RigidBody): lightyear warns
/// against one on an interpolated entity, since Avian would then simulate something the server has
/// already decided.
fn give_bodies(
    arrived: Query<(Entity, &Prop, Has<Predicted>), Arrived>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (entity, prop, predicted) in arrived.iter() {
        let colour = if predicted {
            Color::srgb(0.75, 0.55, 0.3)
        } else {
            Color::srgb(0.35, 0.45, 0.7)
        };
        let mut crate_ = commands.entity(entity);
        crate_.insert((
            Name::from("Crate"),
            Mesh3d(meshes.add(Cuboid::from_size(prop.half_extents * 2.0))),
            MeshMaterial3d(materials.add(colour)),
            prop.collider(),
        ));
        // A prop this client predicts has to be simulated here as well, so it needs the body the
        // server gave it. One it only interpolates gets no `RigidBody` at all — Avian would then
        // simulate something the server has already decided, and lightyear warns against exactly
        // that.
        match (predicted, prop.density) {
            (true, Some(density)) => {
                crate_.insert((RigidBody::Dynamic, ColliderDensity(density)));
                info!("simulating a loose crate");
            }
            (true, None) => warn!("a predicted prop arrived with no density: nothing will move it"),
            (false, _) => {
                info!("drawing a moving crate");
            }
        }
    }
}
