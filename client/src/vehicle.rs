//! Draws the vehicles.
//!
//! Nothing here simulates anything. A vehicle arrives as a [`VehicleKind`] — sent once — and an
//! Avian [`Position`] and [`Rotation`] that lightyear interpolates; this module gives it a body to
//! be seen as and four wheels to stand on.
//!
//! The wheels are the interesting part. They are not replicated and never will be: where a wheel
//! sits is a pure function of the chassis pose and the ground under it, so a client that has the
//! pose can work it out with one ray each. Sending four wheel states per vehicle per update to save
//! four rays per frame would cost bandwidth to save nothing.
//!
//! It also means the wheels move at frame rate rather than at tick rate, over the *interpolated*
//! pose — so they follow the body exactly, with none of the stepping that a replicated wheel
//! position would have.

use avian3d::prelude::{CollisionLayers, LayerMask, Position, RigidBody, Rotation, SpatialQuery};
use bevy::prelude::*;
use lightyear::prelude::client;
use noob_tube_shared::physics::Layer;
use noob_tube_shared::vehicle::{probe_wheels, VehicleKind, Wheels, WHEELS};

/// A vehicle that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<VehicleKind>);

pub struct VehiclePlugin;

impl Plugin for VehiclePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Wheel>()
            .add_systems(Update, (give_bodies, place_wheels).chain());
    }
}

/// Which of the four struts this wheel hangs from.
#[derive(Component, Clone, Copy, Debug, Reflect)]
#[reflect(Component)]
struct Wheel(usize);

/// Update: gives an arrived vehicle a body, four wheels, and something to be hit.
///
/// The collider is inserted for the same reason a crate's is: so that the client tests a shot
/// against the identical shape the server does, rather than rebuilding one from the size and hoping
/// the two agree. It carries no `RigidBody` — this vehicle is interpolated, and Avian would then be
/// simulating something the server has already decided.
fn give_bodies(
    arrived: Query<(Entity, &VehicleKind), Arrived>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for (entity, kind) in arrived.iter() {
        let spec = kind.spec();
        let paint = materials.add(StandardMaterial {
            base_color: Color::srgb(0.70, 0.42, 0.16),
            perceptual_roughness: 0.6,
            ..default()
        });
        let rubber = materials.add(StandardMaterial {
            base_color: Color::srgb(0.09, 0.09, 0.10),
            perceptual_roughness: 0.95,
            ..default()
        });
        // Bevy's cylinder stands on its Y axis; a wheel turns about X, so the mesh is laid on its
        // side once here rather than at every placement.
        let tyre = meshes.add(Cylinder::new(spec.wheel_radius, spec.wheel_width));
        let lay_flat = Quat::from_rotation_z(core::f32::consts::FRAC_PI_2);

        commands.entity(entity).insert((
            Name::from("Buggy"),
            Mesh3d(meshes.add(Cuboid::from_size(spec.half_extents * 2.0))),
            MeshMaterial3d(paint),
            spec.collider(),
            RigidBody::Static,
            // The layer the server gives it. The default would be the same — see `client::props`
            // — but a vehicle is the thing most likely to grow a second collider later, and this
            // is where the answer for it belongs.
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
            Wheels::default(),
        ));
        for index in 0..WHEELS {
            commands.spawn((
                Name::from(format!("Wheel {index}")),
                Wheel(index),
                Mesh3d(tyre.clone()),
                MeshMaterial3d(rubber.clone()),
                // Placed properly on the first frame by `place_wheels`; this only has to be
                // somewhere while the transform propagation catches up.
                Transform::from_translation(spec.mounts[index]).with_rotation(lay_flat),
                ChildOf(entity),
            ));
        }
        info!("drawing a {kind:?}");
    }
}

/// Update: hangs each wheel as far down its strut as the ground allows.
///
/// Purely cosmetic — it applies no force and moves nothing but the wheel meshes, so it is safe to
/// run every frame on a vehicle this client does not simulate.
///
/// The wheel's local position needs only the compression: a strut hangs straight down the chassis's
/// own −Y, so `mount − (rest − compression)` is where the wheel centre goes, in the chassis's own
/// frame. Which is why these are children of the body rather than free entities placed in world
/// space — the parent's transform does the rest, including the interpolation.
fn place_wheels(
    space: SpatialQuery,
    mut vehicles: Query<(Entity, &VehicleKind, &Position, &Rotation, &mut Wheels)>,
    mut wheels: Query<(&Wheel, &ChildOf, &mut Transform)>,
) {
    for (entity, kind, position, rotation, mut found) in vehicles.iter_mut() {
        found.0 = probe_wheels(kind.spec(), position.0, rotation.0, &space, entity);
    }

    for (wheel, parent, mut transform) in wheels.iter_mut() {
        let Ok((_, kind, .., found)) = vehicles.get(parent.parent()) else {
            continue;
        };
        let spec = kind.spec();
        let drop = spec.rest_length - found.0[wheel.0].compression;
        transform.translation = spec.mounts[wheel.0] + Vec3::NEG_Y * drop;
    }
}
