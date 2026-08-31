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

use avian3d::prelude::{ColliderDensity, CollisionLayers, LayerMask, RigidBody};
use bevy::prelude::*;
use lightyear::prelude::{Predicted, client};
use noob_tube_shared::physics::Layer;
use noob_tube_shared::props::{Density, Prop};

/// A prop that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<Prop>);
/// A prop this client has just been asked to predict.
type NowPredicted = (With<Prop>, Added<Predicted>);

pub struct PropsPlugin;

impl Plugin for PropsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (give_bodies, fit_for_the_solver).chain());
    }
}

/// Update: gives an arrived prop something to be seen as, and something to be hit.
///
/// One mesh per prop rather than a shared one, because each is sized from its own shape. There are
/// two of them; sharing by size belongs with the asset handling in M2.
///
/// The collider is here so that a client asks the same question of a prop that the server does —
/// `Hitbox::new(collider, pose)` on both sides — rather than rebuilding a shape from the size and
/// hoping the two agree.
///
/// [`RigidBody::Static`] on something the server moves looks wrong and is not. `MoveAndSlide` only
/// sees colliders attached to a rigid body — its query is filtered `With<ColliderOf>` — so a bare
/// collider is invisible to a player's feet while staying visible to a ray. Static rather than
/// kinematic because Avian never integrates a static body: lightyear writes the pose, Avian only
/// keeps the collider tree in step with it, and there is no second opinion about where the crate is.
fn give_bodies(
    arrived: Query<(Entity, &Prop), Arrived>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (entity, prop) in arrived.iter() {
        commands.entity(entity).insert((
            Name::from("Crate"),
            Mesh3d(meshes.add(Cuboid::from_size(prop.half_extents * 2.0))),
            MeshMaterial3d(materials.add(Color::srgb(0.55, 0.42, 0.28))),
            prop.collider(),
            RigidBody::Static,
            // The same layer the server gives it, written out rather than left to the default.
            // Avian's default membership is the *first* layer, which is `Layer::Body` — deliberately
            // so, because forgetting to label something should be the harmless mistake. Saying it
            // anyway keeps this entity a visible mirror of the server's, where it is not optional.
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
        ));
    }
}

/// Update: gives a prop the body it needs for whichever side of the prediction line it is on.
///
/// The line moves at runtime, exactly as it does for a vehicle. A driver has to predict the crates
/// their bumper is about to move, so the server hands them over on the way into the seat and takes
/// them back on the way out; everyone else keeps the interpolated crate and the rewound hitbox that
/// comes with it. See `crates_follow_the_drivers` on the server for what that trade is worth —
/// 245 rollbacks in four seconds, against a median 3.7 cm when somebody else shoots one.
///
/// Predicted, the crate is [`RigidBody::Dynamic`] and Avian moves it; the mass comes off the wire
/// as [`Density`] rather than out of a constant, because a client that weighs a crate differently
/// from the server pushes it somewhere else. Interpolated, it goes back to `Static` and lightyear
/// writes the pose — leaving it dynamic would have Avian fighting the replicated pose every update.
fn fit_for_the_solver(
    took_over: Query<(Entity, &Density), NowPredicted>,
    mut gave_up: RemovedComponents<Predicted>,
    props: Query<(), With<Prop>>,
    mut commands: Commands,
) {
    for (entity, density) in took_over.iter() {
        commands
            .entity(entity)
            .insert((RigidBody::Dynamic, ColliderDensity(density.0)));
    }
    for entity in gave_up.read() {
        if props.get(entity).is_err() {
            continue;
        }
        commands.entity(entity).insert(RigidBody::Static);
    }
}
