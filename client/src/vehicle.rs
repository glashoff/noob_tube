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
use noob_tube_shared::player::{Player, PlayerInput, PlayerState};
use noob_tube_shared::tuning::NetConfig;
use noob_tube_shared::vehicle::{
    self, Controls, Driven, Driving, Righting, VehicleKind, WHEELS, Wheels, probe_wheels,
};

/// The visual model, under the asset directory.
///
/// Not in the repository, and `/assets/` is gitignored for the reason already written down there:
/// licensed for use, not for redistribution. Each developer brings their own copy. That is also why
/// nothing here may *require* it — see [`give_bodies`], which falls back to a box.
const MODEL: &str = "models/warthog.glb";

/// How long the model is along its own X axis, in its own units.
///
/// Read out of the file rather than guessed: the glTF is a Sketchfab export normalised into a
/// 2 x 0.893 x 0.994 box, so every other number here is a ratio against this one. If the file is
/// ever replaced, these three constants are what has to be measured again.
const MODEL_LENGTH: f32 = 2.0;
/// How far the model's origin sits above the point its tyres touch, in its own units.
const MODEL_GROUND: f32 = 0.446;
/// Which way the model faces. Its windscreen and steering wheel are at −X and its antenna at +X, so
/// its nose points along −X where everything in this game points along −Z: a quarter turn.
const MODEL_YAW: f32 = -core::f32::consts::FRAC_PI_2;

/// A vehicle that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<VehicleKind>);
/// A vehicle this client has just been asked to predict, whether or not it will be steering it.
type NowPredicted = (With<VehicleKind>, Added<Predicted>);
/// This client's own player, while they are behind a wheel.
type OwnDriver = (With<Predicted>, With<Driving>);
/// Any vehicle with somebody in it, this client's own included.
///
/// Narrowing this to *the driver's own* is done by comparing [`Driven`] against the `Player` on
/// this client's own body, not by a filter. `Predicted` used to do it, back when the only vehicle a
/// client predicted was the one it drove — parked ones are predicted now too, so that a driver does
/// not ram an immovable copy of one, and it would identify the whole car park. It also stops
/// identifying anything at all when
/// [`predict_vehicles`](noob_tube_shared::tuning::NetConfig::predict_vehicles) is off, which is
/// exactly the case the comparison has to survive.
type Occupied = (With<VehicleKind>, With<Driven>);

pub struct VehiclePlugin;

impl Plugin for VehiclePlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Wheel>()
            .add_systems(
                Update,
                (give_bodies, fit_for_the_solver, place_wheels).chain(),
            )
            .add_systems(
                FixedUpdate,
                // The same order the server uses: the controls are read before they are acted on,
                // and the driver is put in the seat after the vehicle has moved.
                (
                    take_the_wheel,
                    hold_the_course,
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
    assets: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    // Asked of the filesystem once per arrival rather than of the asset server, because the two
    // answer different questions. The asset server would report a missing file asynchronously,
    // some frames after the vehicle has already been given a body, and by then the choice between
    // the model and the box has been made. This is the same thing the config loader does with the
    // settings file, for the same reason.
    let modelled = std::path::Path::new(crate::ASSETS).join(MODEL).exists();
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
            // Without a mesh of its own this entity has no visibility of its own either, and a
            // child that has one under a parent that does not is a Bevy warning and an
            // inconsistency waiting to happen.
            Visibility::default(),
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
        if modelled {
            // Scaled by its length, so the model and the shape a shot is tested against are the
            // same size along the axis a driver notices most. Lifted so the tyres it is drawn with
            // touch the ground the springs settle it at, rather than the model's own origin doing.
            let scale = spec.half_extents.z * 2.0 / MODEL_LENGTH;
            commands.entity(entity).with_child((
                Name::from("Buggy model"),
                WorldAssetRoot(assets.load(GltfAssetLabel::Scene(0).from_asset(MODEL))),
                Transform::from_xyz(0.0, MODEL_GROUND * scale - spec.ride_height(), 0.0)
                    .with_rotation(Quat::from_rotation_y(MODEL_YAW))
                    .with_scale(Vec3::splat(scale)),
            ));
        } else {
            // No model on this machine: a box the right size, so the vehicle is still visible and
            // still the shape the collider says it is.
            commands.entity(entity).insert((
                Mesh3d(meshes.add(Cuboid::from_size(spec.half_extents * 2.0))),
                MeshMaterial3d(paint),
            ));
        }
        for index in 0..WHEELS {
            commands.spawn((
                Name::from(format!("Wheel {index}")),
                Wheel(index),
                Mesh3d(tyre.clone()),
                MeshMaterial3d(rubber.clone()),
                // Placed properly on the first frame by `place_wheels`; this only has to be
                // somewhere while the transform propagation catches up.
                Transform::from_translation(spec.mounts[index]).with_rotation(lay_flat),
                // Placed every frame by `place_wheels` either way, because where a wheel sits is
                // worth knowing whether or not it is being drawn — the debug view and any future
                // skid mark reads it. Hidden when the model is used, which brings its own tyres.
                if modelled {
                    Visibility::Hidden
                } else {
                    Visibility::Inherited
                },
                ChildOf(entity),
            ));
        }
        info!("drawing a {kind:?}");
    }
}

/// Update: gives a vehicle the body it needs for whichever side of the line it is on.
///
/// The line is prediction, and it moves at runtime: while somebody is driving, the server hands
/// them both the vehicle they are steering *and* every driverless one they might hit, so the same
/// entity is interpolated one moment and predicted the next. Interpolated, it is
/// [`RigidBody::Static`] and lightyear writes its pose. Predicted, it has to be simulated here — so
/// it needs the mass and the centre of gravity the server gave it, and Avian has to be allowed to
/// move it.
///
/// This does not care which of the two a vehicle is. A parked one needs its suspension stepped and
/// its weight honoured exactly as much as a driven one does, because the whole point of predicting
/// it is that the bumper meets the same box on both sides. What separates them is only where the
/// input goes, and that is [`take_the_wheel`]'s business.
///
/// Getting the swap wrong is silent in the worst way. A predicted vehicle left static would take
/// the throttle and not move; an interpolated one left dynamic would fall through the replicated
/// pose being written on top of it every update.
fn fit_for_the_solver(
    net: Res<NetConfig>,
    took_over: Query<(Entity, &VehicleKind), NowPredicted>,
    mut gave_up: RemovedComponents<Predicted>,
    kinds: Query<&VehicleKind>,
    mut commands: Commands,
) {
    // What a predicted chassis is allowed to notice. Everything, or the level alone — see
    // [`VehiclePrediction::World`]: a client can compute where the level is without being told, and
    // cannot compute what somebody else's bumper is about to do, so this is the line drawn in the
    // one place the solver reads it.
    let against = if net.predict_vehicles.simulates_contacts() {
        LayerMask::ALL
    } else {
        Layer::Level.into()
    };
    for (entity, kind) in took_over.iter() {
        let spec = kind.spec();
        commands.entity(entity).insert((
            RigidBody::Dynamic,
            ColliderDensity(spec.density()),
            CenterOfMass(Vec3::NEG_Y * spec.centre_of_mass_drop),
            CollisionLayers::new(Layer::Body, against),
            // `Controls` is deliberately not inserted here: it is replicated, so it arrives with
            // the vehicle, and writing a default over it would be this system racing the wire for
            // a value only the server knows.
            //
            // `Righting` is the opposite case and starts at zero on both sides. A vehicle handed
            // over while it is already on its roof waits out the delay again on this client, which
            // costs a second and a half once and saves replicating a counter nobody else wants.
            Righting::default(),
        ));
        info!("simulating a {kind:?} for myself");
    }
    for entity in gave_up.read() {
        if kinds.get(entity).is_err() {
            continue;
        }
        commands
            .entity(entity)
            // Back to noticing everything. Interpolated, it is not simulated here at all, and the
            // filter only decides what a shot's shape test and other people's feet can see — which
            // is everything, as it is on the server.
            .insert((
                RigidBody::Static,
                CollisionLayers::new(Layer::Body, LayerMask::ALL),
            ))
            .remove::<Righting>();
        info!("a vehicle went back to being interpolated");
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
///
/// The query is [`OwnVehicle`] rather than everything predicted, and that is not a tidy-up. A
/// client predicts the parked vehicles as well, so that its bumper meets the same box the server's
/// does; feeding them this input would have the throttle drive every vehicle on the map.
fn take_the_wheel(
    time: Res<Time<Fixed>>,
    driver: Option<Single<(&Player, &ActionState<PlayerInput>), OwnDriver>>,
    mut vehicles: Query<(&VehicleKind, &Driven, &mut Controls), Occupied>,
) {
    let Some(driver) = driver else {
        return;
    };
    let (me, action) = *driver;
    let dt = time.delta_secs();
    for (kind, driven, mut controls) in vehicles.iter_mut() {
        if driven.0 != me.peer {
            continue;
        }
        controls.apply_input(kind.spec(), &action.0, dt);
    }
}

/// FixedUpdate: keeps a vehicle this client predicts but does not drive turning the way it was.
///
/// The other half of [`take_the_wheel`], and the thing replicating [`Controls`] was for. The
/// throttle and the handbrake are held at whatever last arrived, which is the best a peer can do
/// about somebody else's key; the steering is *continued* rather than held, because the intent
/// travels beside the angle and the easing is the same on both sides. A driver who is holding full
/// left is predicted through the whole turn; one who lets go is predicted wrongly for exactly as
/// long as it takes that news to arrive.
///
/// Deliberately after `take_the_wheel` and filtered against it, so the vehicle this client steers
/// is never eased twice in a tick.
fn hold_the_course(
    time: Res<Time<Fixed>>,
    driver: Option<Single<&Player, OwnDriver>>,
    mut vehicles: Query<(&VehicleKind, Option<&Driven>, &mut Controls), With<Predicted>>,
) {
    let mine = driver.map(|me| me.peer);
    let dt = time.delta_secs();
    for (kind, driven, mut controls) in vehicles.iter_mut() {
        if driven.map(|driven| driven.0) == mine && mine.is_some() {
            continue;
        }
        controls.ease(kind.spec(), dt);
    }
}

/// FixedPostUpdate: a driver is wherever their vehicle ended up.
///
/// The mirror of the server's own, and it has to exist: the walking step skips a seated player, so
/// without this their predicted position would be left wherever they got in — and that position is
/// what the camera stands at and what a shot leaves from.
///
/// **Only for a vehicle this client simulates.** Deriving a predicted player's position from an
/// *interpolated* vehicle is a contradiction, and a measured one: the client would place the driver
/// where the vehicle was a round trip ago while the server places them where it is now, and the two
/// disagree on every update — 35 rollbacks a second, measured, for a picture that never moved. When
/// nothing is predicted the seat is drawn by [`sit_in_the_seat`] instead, and the simulated
/// `PlayerState` is left to be exactly what the server last said it was, which is the only value it
/// can agree on.
fn carry_driver(
    vehicles: Query<(&Position, &Rotation, &Driven), (Occupied, With<Predicted>)>,
    driver: Option<Single<(&Player, &mut PlayerState), OwnDriver>>,
) {
    let Some(driver) = driver else {
        return;
    };
    let (me, mut state) = driver.into_inner();
    let Some((position, rotation, _)) = vehicles.iter().find(|(.., driven)| driven.0 == me.peer)
    else {
        return;
    };
    state.position = position.0 + rotation.0 * Vec3::new(0.0, -0.4, 0.0);
    state.velocity = Vec3::ZERO;
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

/// PostUpdate: puts a driver in the seat of a vehicle nobody predicts.
///
/// Scheduled by `local_player`, chained immediately before the camera reads the seat. It is
/// registered there rather than here so that the ordering is a `chain` rather than two independent
/// `after`s on the same set — an ambiguity in this schedule has already cost this project two bugs,
/// and "the camera reads what this wrote" is exactly the kind of thing that is true until it is
/// silently not.
///
/// The picture-only half of [`carry_driver`], and it exists for the case where there is nothing to
/// simulate: with
/// [`predict_vehicles`](noob_tube_shared::tuning::NetConfig::predict_vehicles) off, the vehicle is
/// interpolated and the driver is a passenger of something the server decides.
///
/// PostUpdate rather than the fixed step is the whole point. Interpolation moves the vehicle at
/// frame rate, so reading it here follows it smoothly; the same read one schedule earlier would
/// quantise the camera to the tick rate and, worse, would feed a rollback comparison a value that
/// cannot match. Writing it *after* the tick has been judged is what makes it cost nothing.
pub(crate) fn sit_in_the_seat(
    vehicles: Query<(&Position, &Rotation, &Driven), (Occupied, Without<Predicted>)>,
    driver: Option<Single<(&Player, &mut PlayerState), OwnDriver>>,
) {
    let Some(driver) = driver else {
        return;
    };
    let (me, mut state) = driver.into_inner();
    let Some((position, rotation, _)) = vehicles.iter().find(|(.., driven)| driven.0 == me.peer)
    else {
        return;
    };
    state.position = position.0 + rotation.0 * Vec3::new(0.0, -0.4, 0.0);
    state.velocity = Vec3::ZERO;
}
