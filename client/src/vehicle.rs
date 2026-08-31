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

use avian3d::prelude::{
    CenterOfMass, ColliderDensity, CollisionLayers, LayerMask, PhysicsSystems, Position, RigidBody,
    Rotation, SpatialQuery, SpeculativeMargin,
};
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::{Predicted, client};
use noob_tube_shared::physics::Layer;
use noob_tube_shared::player::{PlayerInput, PlayerState};
use noob_tube_shared::vehicle::{
    self, probe_wheels, Controls, Driving, Righting, VehicleKind, Wheels, WHEELS,
};

/// A vehicle that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<VehicleKind>);
/// A vehicle this client has just been handed to drive, or has just given back.
type NowDriven = (With<VehicleKind>, Added<Predicted>);
/// This client's own player, while they are behind a wheel.
type OwnDriver = (With<Predicted>, With<Driving>);
/// The one vehicle this client simulates for itself, which is the one it is driving.
type OwnVehicle = (With<VehicleKind>, With<Predicted>);

pub struct VehiclePlugin;

impl Plugin for VehiclePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Wheel>()
            .add_systems(Update, (give_bodies, fit_for_driving, place_wheels).chain())
            .add_systems(
                FixedUpdate,
                // The same order the server uses: the controls are read before they are acted on,
                // and the driver is put in the seat after the vehicle has moved.
                (
                    take_the_wheel,
                    vehicle::drive_vehicles::<With<Predicted>>,
                    vehicle::right_flipped_vehicles::<With<Predicted>>,
                )
                    .chain(),
            )
            // Explicitly after the solver, because Avian runs in this schedule too. Without the
            // ordering the two are ambiguous and the driver is placed at the vehicle's pose from
            // *before* the step on some runs — a tick's worth of the vehicle's speed, which at
            // 13 m/s is 20 cm of position error arriving as a rollback every update. The server has
            // the same ordering for the same reason.
            .add_systems(
                FixedPostUpdate,
                carry_driver.after(PhysicsSystems::StepSimulation),
            );
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
            // Inert while the vehicle is interpolated, and the two sides have to agree the moment
            // it is not — see `vehicle_body` on the server, which says the same thing.
            SpeculativeMargin(vehicle::SPECULATIVE_MARGIN),
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

/// Update: gives a vehicle the body it needs for whichever side of the line it is on.
///
/// The line is prediction, and it moves at runtime: the server hands a vehicle to whoever climbs
/// into it, so the same entity is interpolated one moment and predicted the next. Interpolated, it
/// is [`RigidBody::Static`] and lightyear writes its pose. Predicted, it has to be simulated here —
/// so it needs the mass and the centre of gravity the server gave it, and Avian has to be allowed
/// to move it.
///
/// Getting the swap wrong is silent in the worst way. A predicted vehicle left static would take
/// the throttle and not move; an interpolated one left dynamic would fall through the replicated
/// pose being written on top of it every update.
fn fit_for_driving(
    took_over: Query<(Entity, &VehicleKind), NowDriven>,
    mut gave_up: RemovedComponents<Predicted>,
    kinds: Query<&VehicleKind>,
    mut commands: Commands,
) {
    for (entity, kind) in took_over.iter() {
        let spec = kind.spec();
        commands.entity(entity).insert((
            RigidBody::Dynamic,
            ColliderDensity(spec.density()),
            CenterOfMass(Vec3::NEG_Y * spec.centre_of_mass_drop),
            Controls::default(),
            // Starts at zero on both sides. A vehicle handed over while it is already on its roof
            // waits out the delay again on this client, which costs a second and a half once and
            // saves having to replicate a counter that is otherwise nobody's business.
            Righting::default(),
        ));
        info!("driving a {kind:?}");
    }
    for entity in gave_up.read() {
        if kinds.get(entity).is_err() {
            continue;
        }
        commands
            .entity(entity)
            .insert(RigidBody::Static)
            .remove::<(Controls, Righting)>();
        info!("gave the wheel back");
    }
}

/// FixedUpdate: hands the vehicle this client's own input.
///
/// The one place the two sides differ from each other, and only in how the driver is found. The
/// server looks up who is in the seat; a client does not have to, because the only vehicle it
/// predicts is the one it is driving. Everything downstream reads [`Controls`] and cannot tell.
///
/// The input comes from the `ActionState` rather than from this frame's keyboard, because that is
/// what lightyear refills while replaying a rollback. Reading the keyboard here would replay twenty
/// ticks of steering with whatever is being held *now*.
fn take_the_wheel(
    time: Res<Time<Fixed>>,
    driver: Option<Single<&ActionState<PlayerInput>, OwnDriver>>,
    mut vehicles: Query<(&VehicleKind, &mut Controls), With<Predicted>>,
) {
    let Some(driver) = driver else {
        return;
    };
    let dt = time.delta_secs();
    for (kind, mut controls) in vehicles.iter_mut() {
        controls.apply_input(kind.spec(), &driver.0, dt);
    }
}

/// FixedPostUpdate: a driver is wherever their vehicle ended up.
///
/// The mirror of the server's own, and it has to exist: the walking step skips a seated player, so
/// without this their predicted position would be left wherever they got in — and that position is
/// what the camera stands at and what a shot leaves from.
fn carry_driver(
    vehicle: Option<Single<(&Position, &Rotation), OwnVehicle>>,
    driver: Option<Single<&mut PlayerState, OwnDriver>>,
) {
    let (Some(vehicle), Some(mut driver)) = (vehicle, driver) else {
        return;
    };
    let (position, rotation) = *vehicle;
    driver.position = position.0 + rotation.0 * Vec3::new(0.0, -0.4, 0.0);
    driver.velocity = Vec3::ZERO;
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
