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
//!
//! When there is a model, our own four cylinders are hidden and the model's own wheels are moved
//! instead — see [`find_the_models_wheels`] and [`swing_the_models_wheels`]. The numbers are the
//! same ones; only the thing being drawn changes.

use avian3d::prelude::{
    CenterOfMass, ColliderDensity, CollisionLayers, LayerMask, PhysicsSystems, Position, RigidBody,
    Rotation, SpatialQuery, SpeculativeMargin,
};
use bevy::camera::primitives::Aabb;
use bevy::math::Vec3A;
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;
use lightyear::prelude::{Predicted, client};
use noob_tube_shared::physics::Layer;
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState};
use noob_tube_shared::shooting;
use noob_tube_shared::terrain::Ground;
use noob_tube_shared::tuning::NetConfig;
use crate::local_player::LocalPlayer;
use noob_tube_shared::vehicle::{
    self, Controls, Driven, Driving, FRONT_WHEELS, Righting, SEATED_FEET, VehicleKind, WHEELS,
    Wheels,
    probe_wheels,
};

/// The visual model, under the asset directory.
///
/// In the repository: it is CC BY, and the credit that condition asks for is in
/// `assets/CREDITS.md`. It was not always, which is why nothing here *requires* it — see
/// [`give_bodies`], which falls back to a box. That fallback is worth keeping anyway: the shape a
/// vehicle collides and is shot with does not come from this file — see
/// [`noob_tube_shared::vehicle_shape`] — so a missing model costs the look and nothing else.
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

/// The gun bolted to the cross-beam behind the seats, under the asset directory.
///
/// Optional exactly as the vehicle model is, and for a second reason on top of the first: it is
/// licensed **CC BY-ND**, so unlike the Warthog it is not ours to pass on. See `assets/CREDITS.md`.
const GUN: &str = "models/machine_gun.glb";
/// How long the gun is along its own X, muzzle to the back of its mount, in its own units.
///
/// Read out of the file like the vehicle's three, and its scene is not normalised the way a
/// Sketchfab export usually is — the number is large because the units are. What matters is only
/// that everything below is a ratio against it.
const GUN_LENGTH: f32 = 63.74;
/// How long we want it to be, in metres. About a third of the vehicle, which is what a
/// pintle-mounted gun is against the truck under it.
const GUN_WANTED_LENGTH: f32 = 1.2;
/// The foot of its pintle, in its own units: the one point that has to end up on the beam.
///
/// Measured rather than taken as the origin, which is somewhere in the middle of the receiver. The
/// bottom two units of the model are a single 2.1-wide post, and this is the centre of its underside.
const GUN_FOOT: Vec3 = Vec3::new(3.50, -11.21, 0.0);
/// Which way the gun faces with nobody in the seat, within the vehicle model's own frame.
///
/// Its barrel points along its own +X. The vehicle model's nose is at −X, and half a turn is what
/// puts one on the other — the vehicle's own quarter turn then carries both to −Z together. With
/// somebody driving it is [`aim_the_gun`] that decides, and this is only where it rests.
const GUN_YAW: f32 = core::f32::consts::PI;
/// The end of the barrel, in the gun's own units: where a tracer comes out.
///
/// The barrel is a tube along +X ending at x = 42.06, and this is the centre of its last ring of
/// vertices. Low-poly tubes carry rings only at their ends, which is why the middle of the barrel
/// has no vertices at all and the far end has them all.
const GUN_MUZZLE: Vec3 = Vec3::new(42.06, 3.33, 0.0);
/// How far the gun may be raised, in radians.
///
/// Not a matter of taste. Everything turns about [`GUN_FOOT`], the bottom of the stock is 25.2
/// units behind that point and 10.8 above it, and 23.3° is where the one swings down onto the
/// plane of the other. Past it the gun would put its own tail through the beam it is bolted to.
const GUN_ELEVATION: f32 = 0.40;
/// How far it may be lowered, which is a different number for a different reason.
///
/// What stops the barrel going down is the *front* hoop of the roll cage, not the beam behind it.
/// The gun stands on the rear hoop and fires forward over a single centre rail and then over that
/// hoop, clearing it by 27 cm at rest; measured against the model's own silhouette on the barrel's
/// centreline, the barrel reaches it at **18.4°**.
///
/// Thirty degrees is deliberately past that, and it is the one number here that is a choice rather
/// than a measurement. Stopping at 18.4° puts the nearest ground a driver can hit 5.1 m in front of
/// their own bumper — `1.70 / tan 18.4°`, from a muzzle that stands 1.70 m up — and that dead ring
/// is felt on every pass. Thirty brings it to 2.9 m and costs 13 cm of barrel inside one tube of
/// the cage at full depression: a clip seen occasionally against a hole felt constantly. See the
/// README, and change it here if the trade ever reads differently.
const GUN_DEPRESSION: f32 = 0.52;
/// Where the driver sits, in chassis space: the point their feet go.
///
/// Measured off the model rather than eyeballed. The steering wheel's own node sits at
/// (−0.519, 0.045, −0.428) once the model→chassis map is applied, which puts the driver on the
/// vehicle's **left** — forward is −Z and up is +Y, so +X is the right-hand side. The seat itself
/// is a little behind the wheel, at the `Interior` node's z. The height is not the cab floor: it
/// is whatever puts his *backside on the cushion*, because that is the contact a person reads,
/// and where the feet then land is left to fall where it falls — 7 cm below the floor of the
/// cab, as it turns out. Legs through a footwell nobody can see beat a driver hovering above
/// his own seat, which is what the alternative looked like.
///
/// Distinct from [`SEATED_FEET`], which [`carry_driver`] writes into `PlayerState`. That one is
/// deliberately on the centreline: it is where a shot leaves from and where the camera stands, and
/// putting *those* off to one side would give the driver a view out of the passenger's ear. This
/// is only where the body is drawn.
pub(crate) const DRIVER_SEAT: Vec3 = Vec3::new(-0.516, SEAT_CUSHION - BACKSIDE, -0.039);

/// The seat cushion directly under the driver, in chassis space.
///
/// From the model's own vertices, not from a bounding box. A box round the seat mesh has its top
/// at the seat *backs*, 50 cm higher, and reading the height off one put the driver 11 cm above the
/// cushion with the arithmetic insisting he was 33 cm inside it. The cushion also slopes: −0.22 at
/// its front lip, −0.33 at the back. This is the height at the driver's own z.
const SEAT_CUSHION: f32 = -0.3016;

/// How far the seated body's backside sits above the character's own root.
///
/// The lowest skinned vertex under the pelvis and the backs of the thighs, with the driving clip
/// evaluated and the vertices moved the way the GPU moves them — a bone is inside the flesh, and
/// the question "does he touch the cushion" is about the skin. Scaled by the same 1.70/1.78 the
/// model is drawn at. The same calculation puts the hip bone 0.5584 m above the root, against
/// 0.559 measured in the running game, which is what says the sums are right.
const BACKSIDE: f32 = 0.3842;

/// The top of the cross-beam behind the seats, in the vehicle model's own units.
///
/// Found by looking for what the geometry actually is rather than by eye: the roll cage is the only
/// thing on the body above y = 0.28, it is two full-width hoops joined by a pair of thin rails, and
/// this is the middle of the top face of the rear one. The seat backs end at x = 0.24, so the hoop
/// stands right behind them.
const GUN_MOUNT: Vec3 = Vec3::new(0.178, 0.360, 0.0);

/// What the model calls the three parts of a wheel assembly, as the prefix of a node's name.
///
/// The only thing about the file that is taken on trust rather than measured. Everything else —
/// which corner a part belongs to, how big the tyre is, where an arm is bolted — is worked out from
/// where the geometry actually is, so that re-exporting the model cannot quietly move a wheel to
/// the wrong strut. A name, though, is the one thing geometry cannot tell you: a tyre and the hub
/// inside it are two boxes in the same place.
const MODEL_TYRE: &str = "Tire";
const MODEL_AXLE: &str = "Axel";
const MODEL_ARM: &str = "Suspension";

/// A vehicle that has just arrived and has nothing to be seen as yet.
type Arrived = (With<client::Remote>, Added<VehicleKind>);
/// A vehicle this client has just been asked to predict, whether or not it will be steering it.
type NowPredicted = (With<VehicleKind>, Added<Predicted>);
/// This client's own player, while they are behind a wheel.
///
/// `pub(crate)` because the camera asks the same question: which vehicle to point at is the same
/// lookup as which vehicle to steer, and two spellings of it would be two things to keep in step.
pub(crate) type OwnDriver = (With<Predicted>, With<Driving>);
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
            .register_type::<ModelWheel>()
            .register_type::<Rolling>()
            .register_type::<MountedGun>()
            .add_systems(
                Update,
                (
                    give_bodies,
                    fit_for_the_solver,
                    place_wheels,
                    find_the_models_wheels,
                    swing_the_models_wheels,
                    aim_the_gun,
                )
                    .chain(),
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
                    .chain()
                    // For the same reason the walking step waits: a predicted vehicle with no
                    // ground under it finds no wheels and falls. See `local_player`.
                    .run_if(resource_exists::<Ground>),
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

/// A part of the model that hangs from a strut rather than sitting on the body.
///
/// The model arrives as one rigid scene: its tyres, its stub axles and its suspension arms are
/// nodes of the same tree as its bodywork, all drawn at the pose the artist modelled. Hiding our
/// own cylinders behind it therefore cost the suspension travel — the springs still worked, and
/// none of it showed. This is what puts it back: it says which strut each of those nodes belongs
/// to, so that the compression [`place_wheels`] already computes can be handed to the picture.
#[derive(Component, Clone, Copy, Debug, Reflect)]
#[reflect(Component)]
struct ModelWheel {
    /// The vehicle this hangs from. Stored rather than walked to: the model's nodes are three deep
    /// under the chassis, and the walk would run every frame for an answer that never changes.
    chassis: Entity,
    /// Which strut, indexing [`VehicleSpec::mounts`](noob_tube_shared::vehicle::VehicleSpec::mounts).
    corner: usize,
    /// The point this part turns about, in the model's own units.
    ///
    /// Needed because the geometry is baked where it stands rather than around its own origin — a
    /// Sketchfab export has no local transforms at all — so rotating a node about the origin would
    /// swing a wheel through the bodywork rather than turning it on its hub.
    pivot: Vec3,
    /// What it is, which is what decides how it moves.
    part: Part,
}

/// The three things a wheel assembly is made of.
#[derive(Clone, Copy, Debug, Reflect)]
enum Part {
    /// The tyre: it rises and falls with the strut, steers if it is a front one, and rolls.
    ///
    /// The radius travels with it because it is not quite the radius the struts assume — this
    /// model's tyre comes out 2 cm larger once scaled, which would leave the tread that far under
    /// the floor for as long as the vehicle is on the ground. Trimming it to the spec's radius is
    /// what makes the drawn contact patch the one the simulation is using.
    Tyre { radius: f32 },
    /// The stub axle behind the wheel. It goes wherever the wheel goes and does not roll: it is
    /// what the wheel turns *on*, and it is inside the hub where nobody could see it turn anyway.
    Axle,
    /// The suspension arm. Bolted to the body at one end and holding the wheel at the other, so it
    /// does not travel — it swings, by whatever angle keeps its far end on the wheel.
    ///
    /// The lever is how far the wheel is from that bolt, along the model's own X. Its sign carries
    /// which end of the vehicle the arm is on, so the swing needs no case of its own.
    Arm { lever: f32 },
}

/// On a model, until its wheels have been found.
///
/// The scene spawns some frames after the entity that asks for it, and there is no ordering that
/// makes it otherwise — the glTF has to be read off disk first. So the parts are looked for rather
/// than waited on, and this is removed the moment they turn up. Without it the search would walk
/// every vehicle's whole node tree every frame for the rest of the round.
#[derive(Component)]
struct Unfitted;

/// The gun bolted to the beam, and where its barrel ends.
///
/// `pub(crate)` because a tracer starts here — see `shot_effects`. The muzzle is a point in the
/// gun's own units and this entity is the gun's own frame, so where it is in the world is one
/// `transform_point` away from a `GlobalTransform` and nothing here has to work it out.
#[derive(Component, Clone, Copy, Debug, Reflect)]
#[reflect(Component)]
pub(crate) struct MountedGun {
    /// The vehicle it is bolted to, stored for the same reason [`ModelWheel`] stores it.
    pub(crate) chassis: Entity,
    pub(crate) muzzle: Vec3,
    /// How much of its own size it is drawn at, which [`gun_pose`] needs and nothing else does.
    size: f32,
}

/// Where the gun sits when it is traversed and elevated by those two angles.
///
/// Both turns are about [`GUN_FOOT`] — the one point measured onto the beam — so the gun stays
/// bolted to it however it is aimed. A real pintle would elevate about a trunnion higher up
/// instead, and cannot be had here: the post and the gun are a single mesh in the file, so turning
/// about the trunnion would lift the foot 9 cm out of its socket at full elevation. Turning about
/// the socket is the other kind of mount, and the only one of the two this geometry can be.
fn gun_pose(traverse: f32, pitch: f32, size: f32) -> Transform {
    // Traverse outside elevation: the post turns and the gun tips on it, not the other way about.
    let rotation = Quat::from_rotation_y(traverse) * Quat::from_rotation_z(pitch);
    Transform::from_translation(GUN_MOUNT - rotation * (GUN_FOOT * size))
        .with_rotation(rotation)
        .with_scale(Vec3::splat(size))
}

/// How far a vehicle's wheels have turned.
///
/// Cosmetic and client-only, and derived from how far the chassis has actually moved rather than
/// from its velocity — because for a vehicle this client does not simulate those are two different
/// numbers. An interpolated one is *placed* each frame, between two poses the server sent, and the
/// distance between two placements is the only speed it really has.
#[derive(Component, Clone, Copy, Debug, Default, Reflect)]
#[reflect(Component)]
struct Rolling {
    /// Radians, unbounded and never wrapped. A quaternion does not care how many turns it is given.
    angle: f32,
    /// Where the chassis was when that was last advanced, or `None` on its first frame.
    was: Option<Vec3>,
}

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
    // The gun needs the vehicle model, not merely its own file: what it is bolted to is a beam that
    // only exists in the model. On the box there is nothing to bolt it to.
    let armed = modelled && std::path::Path::new(crate::ASSETS).join(GUN).exists();
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
            // **Given here rather than at the handover, and that is the whole of a bug.** Avian
            // works a body's mass out from its collider and its density, and a body with no
            // `ColliderDensity` is given the default of 1 — which for this hull is about two and a
            // half kilograms instead of twelve hundred. A static body does not care; the first
            // tick as a dynamic one cares enormously, because the suspension pushes with forces
            // sized for the real mass. Measured, when these two arrived in the same frame as
            // `RigidBody::Dynamic`: the buggy left the ground at the moment somebody got in and
            // reached 5.9 m before falling back. Only ever on the *first* handover, because after
            // it these components stayed behind and the second was correct — which is exactly the
            // kind of "it only happens sometimes" that costs an afternoon.
            //
            // Neither is a fact about who predicts the vehicle. They are facts about what a buggy
            // is, so they belong where it is built.
            ColliderDensity(spec.density()),
            CenterOfMass(Vec3::NEG_Y * spec.centre_of_mass_drop),
            // Inert while the vehicle is interpolated, and the two sides have to agree the moment
            // it is not — see `vehicle_body` on the server, which says the same thing.
            SpeculativeMargin(vehicle::SPECULATIVE_MARGIN),
            // The layer the server gives it. The default would be the same — see `client::props`
            // — but a vehicle is the thing most likely to grow a second collider later, and this
            // is where the answer for it belongs.
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
            Wheels::default(),
            Rolling::default(),
        ));
        if modelled {
            // Scaled by its length, so the model and the shape a shot is tested against are the
            // same size along the axis a driver notices most. Lifted so the tyres it is drawn with
            // touch the ground the springs settle it at, rather than the model's own origin doing.
            let scale = spec.half_extents.z * 2.0 / MODEL_LENGTH;
            commands.entity(entity).with_children(|body| {
                let mut model = body.spawn((
                    Name::from("Buggy model"),
                    // Its own wheels are still to be found — see `find_the_models_wheels`, which is
                    // what takes this off again once they have been.
                    Unfitted,
                    WorldAssetRoot(assets.load(GltfAssetLabel::Scene(0).from_asset(MODEL))),
                    Transform::from_xyz(0.0, MODEL_GROUND * scale - spec.ride_height(), 0.0)
                        .with_rotation(Quat::from_rotation_y(MODEL_YAW))
                        .with_scale(Vec3::splat(scale)),
                ));
                if !armed {
                    return;
                }
                // A child of the vehicle model rather than of the chassis, and placed in the
                // model's own units. Where the beam is depends on the model and on nothing else, so
                // hanging the gun off the same entity keeps the two glued together — the vehicle
                // could be respecified tomorrow and the gun would still be on its beam.
                model.with_children(|model| {
                    // Divided by the vehicle model's scale because that is already applied above,
                    // and this is underneath it.
                    let size = GUN_WANTED_LENGTH / GUN_LENGTH / scale;
                    model.spawn((
                        Name::from("Mounted gun"),
                        MountedGun { chassis: entity, muzzle: GUN_MUZZLE, size },
                        WorldAssetRoot(assets.load(GltfAssetLabel::Scene(0).from_asset(GUN))),
                        // Where it rests until somebody sits down — see `aim_the_gun`.
                        gun_pose(GUN_YAW, 0.0, size),
                    ));
                });
            });
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
        commands.entity(entity).insert((
            RigidBody::Dynamic,
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
    state.position = position.0 + rotation.0 * SEATED_FEET;
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
    mut vehicles: Query<(
        Entity,
        &VehicleKind,
        &Position,
        &Rotation,
        &mut Wheels,
        &mut Rolling,
    )>,
    mut wheels: Query<(&Wheel, &ChildOf, &mut Transform)>,
) {
    for (entity, kind, position, rotation, mut found, mut rolling) in vehicles.iter_mut() {
        let spec = kind.spec();
        found.0 = probe_wheels(spec, position.0, rotation.0, &space, entity);
        // And how far they have turned while getting there, which only the model's own tyres are
        // detailed enough to show — a cylinder looks identical whichever way round it is. Measured
        // from the ground the vehicle has covered along its own nose, so reversing unwinds it and
        // sliding sideways with the wheels locked does not turn them at all.
        //
        // Anything longer than the vehicle in one frame did not happen by driving: it is the jump
        // from wherever an entity was spawned to wherever replication says it belongs, which
        // arrives a frame or two into its life and would otherwise spin its wheels a dozen times on
        // the spot. Not moved, so not rolled.
        let step = rolling.was.map_or(Vec3::ZERO, |was| position.0 - was);
        rolling.was = Some(position.0);
        if step.length_squared() < (spec.half_extents.z * 2.0).powi(2) {
            rolling.angle += step.dot(rotation.0 * Vec3::NEG_Z) / spec.wheel_radius;
        }
    }

    for (wheel, parent, mut transform) in wheels.iter_mut() {
        let Ok((_, kind, _, _, found, _)) = vehicles.get(parent.parent()) else {
            continue;
        };
        let spec = kind.spec();
        let drop = spec.rest_length - found.0[wheel.0].compression;
        transform.translation = spec.mounts[wheel.0] + Vec3::NEG_Y * drop;
    }
}

/// Update: finds the model's own wheels, and works out which strut each of them belongs to.
///
/// It reads the answer out of the geometry rather than off a list: a part's corner comes from where
/// it sits (see [`corner_of`]) and its size from the bounding box the glTF loader has already
/// measured from the vertices. The alternative, a table of node names against corners, would be
/// silently wrong the first time somebody re-exported the model with its parts renamed, and wrong
/// in the way that is hardest to see — three wheels right and one crossed over.
///
/// It runs until it finds something rather than being triggered, because there is nothing to
/// trigger on. The scene arrives whenever the file has been read, and a vehicle exists well before
/// that.
fn find_the_models_wheels(
    models: Query<(Entity, &ChildOf), With<Unfitted>>,
    children: Query<&Children>,
    named: Query<&Name>,
    bounds: Query<&Aabb>,
    mut commands: Commands,
) {
    for (model, mounted_on) in models.iter() {
        // Everything named like part of a wheel, with the corner it turned out to be on.
        let mut found: Vec<(Entity, &'static str, usize, Aabb)> = Vec::new();
        for node in children.iter_descendants(model) {
            let Ok(name) = named.get(node) else {
                continue;
            };
            let Some(kind) = [MODEL_TYRE, MODEL_AXLE, MODEL_ARM]
                .into_iter()
                .find(|prefix| name.starts_with(*prefix))
            else {
                continue;
            };
            let Some(box_) = enclosing(node, &children, &bounds) else {
                continue;
            };
            found.push((node, kind, corner_of(Vec3::from(box_.center)), box_));
        }
        // Nothing at all: the scene has not spawned yet, which is the ordinary case for a
        // vehicle's first frames. Try again next frame rather than deciding it has no wheels.
        if found.is_empty() {
            continue;
        }

        // The tyres first, because an arm's swing is measured to the wheel it holds.
        let mut centres = [None; WHEELS];
        for (_, kind, corner, box_) in &found {
            if *kind == MODEL_TYRE {
                centres[*corner] = Some(Vec3::from(box_.center));
            }
        }
        let mut fitted = 0;
        for (node, kind, corner, box_) in found {
            let centre = Vec3::from(box_.center);
            let half = Vec3::from(box_.half_extents);
            let (pivot, part) = match kind {
                // A tyre turns on its own centre, and is round in the model's X–Y plane because its
                // axle lies along the model's Z.
                MODEL_TYRE => (centre, Part::Tyre { radius: half.y }),
                MODEL_AXLE => (centre, Part::Axle),
                // An arm turns on the end that is bolted to the body, which is the one nearer the
                // middle of the vehicle.
                _ => {
                    let inboard = centre.x - half.x * centre.x.signum();
                    let Some(wheel) = centres[corner] else {
                        continue;
                    };
                    let lever = wheel.x - inboard;
                    if lever == 0.0 {
                        continue;
                    }
                    (Vec3::new(inboard, centre.y, centre.z), Part::Arm { lever })
                }
            };
            commands.entity(node).insert(ModelWheel {
                chassis: mounted_on.parent(),
                corner,
                pivot,
                part,
            });
            fitted += 1;
        }
        commands.entity(model).remove::<Unfitted>();
        info!("hung {fitted} of the model's parts on the struts");
    }
}

/// Which strut a part of the model belongs to, from where it sits in the model's own space.
///
/// Turned into chassis space first, where front is −Z and right is +X and the mounts are written in
/// that order — front left, front right, rear left, rear right. Going through [`MODEL_YAW`] rather
/// than reading the model's axes directly is what lets a model exported facing some other way be
/// sorted correctly by changing one constant.
fn corner_of(centre: Vec3) -> usize {
    let at = Quat::from_rotation_y(MODEL_YAW) * centre;
    (if at.z < 0.0 { 0 } else { 2 }) + usize::from(at.x > 0.0)
}

/// The box a glTF node's geometry takes up, in that node's own space.
///
/// A named node holds no geometry itself. Under it the file has a node per mesh, and under *that*
/// the loader hangs one entity per primitive carrying the bounding box it measured from the
/// vertices — so this has to look at everything below, not just the children.
///
/// It reads those boxes as if they were already in the named node's frame, which they are only
/// because this model has no local transforms at all — a Sketchfab export bakes every node's pose
/// into its vertices, and every one of this file's forty-two nodes was checked to be sitting at the
/// identity. A model that did use them would need each box carried up through its own `Transform`,
/// and would be drawn with its wheels in the wrong place until it was.
///
/// `None` while the meshes have yet to arrive, which is not an error — it is what "the scene is
/// still loading" looks like from here.
fn enclosing(node: Entity, children: &Query<&Children>, bounds: &Query<&Aabb>) -> Option<Aabb> {
    let mut min = Vec3A::splat(f32::INFINITY);
    let mut max = Vec3A::splat(f32::NEG_INFINITY);
    for part in children.iter_descendants(node) {
        let Ok(box_) = bounds.get(part) else {
            continue;
        };
        min = min.min(box_.center - box_.half_extents);
        max = max.max(box_.center + box_.half_extents);
    }
    min.cmple(max)
        .all()
        .then(|| Aabb::from_min_max(min.into(), max.into()))
}

/// Update: moves the model's wheels to where the struts say they are.
///
/// The model is drawn at the pose the artist modelled, and that pose is the vehicle standing on its
/// springs under its own weight. So what a wheel needs is not its height but the *difference*
/// between this strut's compression and the compression it has standing still: zero at rest, which
/// is why a parked vehicle looks exactly as it did before any of this existed, and why nothing has
/// to be measured out of the file a second time to place it.
///
/// All of it is arithmetic in the model's own units, left to the model root's scale and yaw to turn
/// into metres. That is what keeps it independent of how big the vehicle is — divide by the scale
/// once, at the top, and nothing below has to know.
///
/// Runs on every vehicle, predicted or interpolated, because none of it applies a force or moves
/// anything but a mesh. [`place_wheels`] has already done the ray casts either way.
fn swing_the_models_wheels(
    vehicles: Query<(&VehicleKind, &Wheels, &Rolling, Option<&Controls>)>,
    mut parts: Query<(&ModelWheel, &mut Transform)>,
) {
    for (part, mut transform) in parts.iter_mut() {
        let Ok((kind, wheels, rolling, controls)) = vehicles.get(part.chassis) else {
            continue;
        };
        let spec = kind.spec();
        let scale = spec.half_extents.z * 2.0 / MODEL_LENGTH;
        // Upwards is positive: a strut squashed harder than the vehicle's own weight squashes it
        // has pushed its wheel up into the arch.
        let travel = (wheels.0[part.corner].compression - spec.static_compression()) / scale;
        // Only the front wheels turn, and they are the first mounts. The angle is the simulation's
        // own, with its own sign, because it is the same rotation about the same axis: a yaw is a
        // yaw whatever else stands between the model and the chassis.
        let steer = match (part.corner < FRONT_WHEELS, controls) {
            (true, Some(controls)) => -controls.steer,
            _ => 0.0,
        };
        let (rotation, trim, lift) = match part.part {
            Part::Tyre { radius } => {
                // Trimmed to the radius the struts assume, and only in the plane the tyre is round
                // in — its width is nobody's business, since nothing is ever collided against it.
                let round = spec.wheel_radius / scale / radius;
                (
                    // Rolling first, then steering: a wheel spins on an axle that the steering
                    // turns, not the other way about.
                    Quat::from_rotation_y(steer) * Quat::from_rotation_z(rolling.angle),
                    Vec3::new(round, round, 1.0),
                    travel,
                )
            }
            Part::Axle => (Quat::from_rotation_y(steer), Vec3::ONE, travel),
            // The bolt stays where it is and the arm swings about it, so this one gets no lift of
            // its own: the rotation is the whole of its movement.
            Part::Arm { lever } => (
                Quat::from_rotation_z((travel / lever).clamp(-1.0, 1.0).asin()),
                Vec3::ONE,
                0.0,
            ),
        };
        // About the part's own pivot rather than the model's origin, which is merely where the
        // vertices happen to be measured from. Bevy composes a transform as scale, then rotation,
        // then translation, so this is the translation that leaves the pivot where it was asked to
        // be and turns everything else around it.
        *transform = Transform {
            translation: part.pivot + Vec3::Y * lift - rotation * (trim * part.pivot),
            rotation,
            scale: trim,
        };
    }
}

/// The traverse and elevation that lay the gun's barrel along a world direction.
///
/// Said in the vehicle model's own frame, which is why the chassis rotation has to come in: the
/// whole chain from the chassis down is already in the transform, and subtracting it once here is
/// what leaves angles that mean the same thing whether the car is turning, leaning on its springs
/// or standing on a slope.
///
/// The barrel is the gun's own +X. A rotation about Y takes +X towards −Z, which is where the sign
/// on `at.z` comes from, and a rotation about Z raises it, which is why the elevation is a plain
/// arcsine of the height.
fn barrel_angles(chassis: Quat, direction: Vec3) -> (f32, f32) {
    let at = (chassis * Quat::from_rotation_y(MODEL_YAW)).inverse() * direction;
    (f32::atan2(-at.z, at.x), at.y.clamp(-1.0, 1.0).asin())
}

/// Whether the gun on a chassis standing like that can be brought to bear on that aim.
///
/// The trigger's half of [`GUN_DEPRESSION`], and it has to be the same number the barrel stops at
/// or the two drift apart: a shot that leaves a gun pointing somewhere else is worse than no shot.
/// Only the depression is asked about — a gun that cannot be raised far enough is still pointing
/// more or less where the driver is looking, while one that cannot be lowered far enough is
/// pointing over the top of what they are aiming at.
///
/// `pub(crate)` for `local_player`, which is where an input is decided. Deliberately a pure
/// function of the pose and the angles rather than a question about the gun *entity*: a client with
/// no model has no gun to ask, and must not thereby be allowed to shoot where nobody else can.
pub(crate) fn can_bear(chassis: Quat, yaw: f32, pitch: f32) -> bool {
    let (_, elevation) = barrel_angles(chassis, shooting::aim_ray(Vec3::ZERO, yaw, pitch).1);
    elevation >= -GUN_DEPRESSION
}

/// Update: points the mounted gun where whoever is driving is looking.
///
/// Two turns about two points — see [`Pintle`]. Both are worked out in the vehicle *model's* own
/// frame rather than in the world's, which is what makes the gun follow a car that is turning,
/// leaning on its springs or standing on a slope: the whole chain from the chassis down is already
/// in the transform, so subtracting it once here leaves angles that mean the same thing at any
/// attitude.
///
/// The elevation is clamped and the traverse is not. A pintle behind the seats can be swung all the
/// way round — that is what it is for — but it cannot be tipped past [`GUN_ELEVATION`] without
/// putting its own stock through the beam, nor past [`GUN_DEPRESSION`] without burying its barrel
/// in the roll cage ahead of it.
///
/// It does not fire. What comes out of the barrel is the driver's own shot, cast from the seat as
/// it always was; this only makes the picture agree with it.
fn aim_the_gun(
    vehicles: Query<(&Rotation, Option<&Driven>)>,
    aims: Query<(&Player, &Aim)>,
    // Two of them, because the look angles and the player are two entities: `LocalPlayer` is on the
    // camera. Asking for both on one — which is what this was — quietly matches nothing at all.
    own: Option<Single<&Player, With<Predicted>>>,
    view: Option<Single<&LocalPlayer>>,
    mut guns: Query<(&MountedGun, &mut Transform)>,
) {
    // For ourselves, the angles the camera is using *this frame* rather than the predicted `Aim`,
    // which is written once per tick and so steps at the tick rate. The gun fills a good part of
    // the screen and the crosshair beside it moves at frame rate; the two visibly disagreeing while
    // panning is the difference between aiming and asking a turret to catch up.
    //
    // Two separate `Single`s because these are two entities: `LocalPlayer` is on the camera and
    // `Player` is on the body. Asked for as one — which is how this was written — the query matches
    // nothing at all, and silently: every gun falls through to the branch below.
    let mine = own
        .zip(view)
        .map(|(who, view)| (who.peer, view.yaw, view.pitch));
    for (gun, mut pose) in guns.iter_mut() {
        let Ok((rotation, driven)) = vehicles.get(gun.chassis) else {
            continue;
        };
        let looking = driven.and_then(|driven| {
            mine.filter(|(peer, ..)| *peer == driven.0)
                .map(|(_, yaw, pitch)| (yaw, pitch))
                .or_else(|| {
                    aims.iter()
                        .find(|(who, _)| who.peer == driven.0)
                        .map(|(_, aim)| (aim.yaw, aim.pitch))
                })
        });
        let (traverse, pitch) = match looking {
            // The same ray the shot is cast down.
            Some((yaw, pitch)) => {
                let (traverse, elevation) =
                    barrel_angles(rotation.0, shooting::aim_ray(Vec3::ZERO, yaw, pitch).1);
                (traverse, elevation.clamp(-GUN_DEPRESSION, GUN_ELEVATION))
            }
            // An empty seat: back to pointing over the bonnet, which is where it started.
            None => (GUN_YAW, 0.0),
        };
        *pose = gun_pose(traverse, pitch, gun.size);
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
    state.position = position.0 + rotation.0 * SEATED_FEET;
    state.velocity = Vec3::ZERO;
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::vehicle::BUGGY;

    /// Where the model's four tyres sit, in its own units, as the glTF measures them.
    ///
    /// A fixture rather than a lookup. The model is in the repository now, but the gun beside it
    /// is not and never will be — and a test that reached into an asset would skip itself when one
    /// was missing, which is a test that passes everywhere and checks nothing. These are the same
    /// four numbers [`MODEL_LENGTH`] and [`MODEL_GROUND`] were read out beside, and replacing the
    /// model means measuring all of them again. `tools/glb nodes` is what reads them back out.
    const TYRES: [(Vec3, usize); WHEELS] = [
        (Vec3::new(-0.6714, -0.2245, 0.3749), 0),
        (Vec3::new(-0.6714, -0.2245, -0.3755), 1),
        (Vec3::new(0.6272, -0.2245, 0.3755), 2),
        (Vec3::new(0.6272, -0.2245, -0.3747), 3),
    ];

    /// Each of the model's wheels has to land on the strut it is drawn over.
    ///
    /// This is the one piece of the fitting that fails quietly. A model whose wheels are hung on
    /// the wrong corners still draws four wheels in four arches, and only moves the wrong one when
    /// the vehicle leans; three right and one crossed over is the shape of the mistake, and
    /// standing still it is invisible. It is [`MODEL_YAW`] that decides it, and a wrong yaw is easy
    /// to arrive at because three of the four quarter turns put the vehicle's nose somewhere
    /// plausible.
    #[test]
    fn the_models_wheels_land_on_the_struts_they_belong_to() {
        for (centre, strut) in TYRES {
            assert_eq!(
                corner_of(centre),
                strut,
                "the tyre at {centre:?} was hung on another corner",
            );
        }
    }

    /// The collision shape was baked with the map the visible model is drawn with.
    ///
    /// `tools/bake_collider` turns the model into hulls in chassis space, and to do that it has to
    /// apply the same quarter turn, the same scale and the same lift that [`give_bodies`] applies
    /// to the model a player looks at. Nothing but this holds the two together: they are written
    /// down in different crates, one of which cannot see the other's constants. Drift between them
    /// puts every bullet hole where the bodywork is not — the bug the bake exists to fix, back
    /// again with a subtler cause.
    ///
    /// If this fails after a model or a spec changed, the fix is to re-bake, not to edit either
    /// number: `cargo run -p bake_collider -- bake assets/models/warthog.glb shared/src/vehicle_shape.rs`.
    #[test]
    fn the_shape_was_baked_with_the_map_the_model_gets() {
        use noob_tube_shared::vehicle_shape;
        let spec = BUGGY;
        let scale = spec.half_extents.z * 2.0 / MODEL_LENGTH;
        let lift = MODEL_GROUND * scale - spec.ride_height();
        assert!(
            (scale - vehicle_shape::BUGGY_BAKE_SCALE).abs() < 1e-4,
            "the model is drawn at {scale} and the shape was baked at {}",
            vehicle_shape::BUGGY_BAKE_SCALE,
        );
        assert!(
            (lift - vehicle_shape::BUGGY_BAKE_LIFT).abs() < 1e-3,
            "the model is lifted {lift} and the shape was baked lifted {}",
            vehicle_shape::BUGGY_BAKE_LIFT,
        );
        // The bake writes the turn out as the axis swap (x, y, z) -> (-z, y, x) rather than as an
        // angle, so what has to match is that this yaw *is* that swap.
        let swapped = Quat::from_rotation_y(MODEL_YAW) * Vec3::new(1.0, 2.0, 3.0);
        assert!(
            swapped.abs_diff_eq(Vec3::new(-3.0, 2.0, 1.0), 1e-5),
            "MODEL_YAW sends (1, 2, 3) to {swapped:?}, but the bake sends it to (-3, 2, 1)",
        );
    }

    /// And the struts have to be written in the order the rule sorts them into: front left, front
    /// right, rear left, rear right. The rule reads the signs; this is what says which mount each
    /// pair of signs means.
    #[test]
    fn the_corner_rule_agrees_with_the_order_the_struts_are_written_in() {
        let into_the_model = Quat::from_rotation_y(-MODEL_YAW);
        for (index, mount) in BUGGY.mounts.iter().enumerate() {
            assert_eq!(
                corner_of(into_the_model * *mount),
                index,
                "strut {index} at {mount:?} was sorted onto another corner",
            );
        }
    }

    /// The rearmost point of the gun, in its own units: the bottom of the stock.
    ///
    /// A fixture for the same reason the tyre positions above are one, and it is what
    /// [`GUN_ELEVATION`] was worked out from — the angle at which this meets the beam.
    const GUN_TAIL: Vec3 = Vec3::new(-21.68, -0.375, 0.0);

    /// The foot of the post has to stay on the beam whatever the driver is looking at. It is the
    /// one thing about the mounting that was measured against the model, and turning about the
    /// wrong point is exactly how it would be given away — invisibly at rest, and by a gun leaning
    /// off its beam the moment somebody looks up.
    #[test]
    fn the_gun_stays_bolted_to_the_beam_however_it_is_aimed() {
        for step in -4..=4 {
            let pitch = GUN_ELEVATION * step as f32 / 4.0;
            for traverse in [0.0, 1.0, GUN_YAW, -2.2] {
                let foot = gun_pose(traverse, pitch, 0.02).transform_point(GUN_FOOT);
                assert!(
                    foot.distance(GUN_MOUNT) < 1e-5,
                    "aimed {traverse}/{pitch} the foot left the beam for {foot:?}"
                );
            }
        }
    }

    /// And it may not swing its own stock through what it is bolted to. This is where
    /// [`GUN_ELEVATION`] comes from, so the test is that the limit is the real one: at it the tail
    /// is clear, and past it the tail is through the beam.
    #[test]
    fn the_gun_cannot_swing_its_stock_through_the_cage() {
        // In the gun's own units, about its own foot: the beam is the plane the foot sits in.
        // In the gun's own units and about its own foot, which is the point on the beam: the
        // beam is then the plane the tail has to stay above.
        let above = |pitch: f32| (Quat::from_rotation_z(pitch) * (GUN_TAIL - GUN_FOOT)).y;
        assert!(above(GUN_ELEVATION) > 0.0, "the stock is already through the beam at the limit");
        assert!(above(GUN_ELEVATION + 0.05) < 0.0, "the limit is beyond what the geometry allows");
        assert!(above(-GUN_DEPRESSION) > 0.0, "lowering the barrel put the stock through the beam");
    }

    /// The barrel has to end up along the direction the driver is looking, whatever the car is
    /// doing underneath it. Two frames stand between the two — the chassis and the model's own
    /// quarter turn — and getting either the wrong way round leaves a gun that tracks the aim
    /// perfectly while pointing somewhere else.
    #[test]
    fn the_gun_points_where_its_driver_is_looking() {
        let barrel = |chassis: Quat, traverse: f32, pitch: f32| {
            chassis
                * Quat::from_rotation_y(MODEL_YAW)
                * Quat::from_rotation_y(traverse)
                * Quat::from_rotation_z(pitch)
                * Vec3::X
        };
        let attitudes = [
            Quat::IDENTITY,
            Quat::from_rotation_y(1.3),
            Quat::from_rotation_y(-2.7),
            // On a slope, and leaning on its springs.
            Quat::from_euler(EulerRot::YXZ, 0.6, 0.2, -0.15),
        ];
        for chassis in attitudes {
            for yaw in [0.0, 1.0, 2.5, -2.0, core::f32::consts::PI] {
                // Inside the elevation the mount actually has; past it the gun cannot follow, and
                // is not meant to.
                for pitch in [0.0, 0.3, -0.3, 1.2, -1.2] {
                    let (_, wanted) = shooting::aim_ray(Vec3::ZERO, yaw, pitch);
                    let (traverse, elevated) = barrel_angles(chassis, wanted);
                    let along = barrel(chassis, traverse, elevated);
                    assert!(
                        along.distance(wanted) < 1e-4,
                        "aimed {yaw}/{pitch} at {wanted:?}, barrel lies along {along:?}"
                    );
                }
            }
        }
    }

    /// With nobody in the seat the gun rests pointing over the bonnet, and that has to be the same
    /// pose aiming straight ahead produces — or the gun would visibly jump the moment the driver
    /// sat down.
    #[test]
    fn an_empty_seat_leaves_the_gun_where_aiming_ahead_would_put_it() {
        let (_, ahead) = shooting::aim_ray(Vec3::ZERO, 0.0, 0.0);
        let (traverse, pitch) = barrel_angles(Quat::IDENTITY, ahead);
        assert!((traverse - GUN_YAW).abs() < 1e-5, "resting at {GUN_YAW}, aimed ahead {traverse}");
        assert!(pitch.abs() < 1e-5, "aiming level is not level: {pitch}");
    }

    /// The trigger has to stop exactly where the barrel does. Two numbers for one limit is how a
    /// shot comes to leave a gun that is pointing somewhere else.
    #[test]
    fn the_trigger_stops_where_the_barrel_stops() {
        let chassis = Quat::from_euler(EulerRot::YXZ, 0.9, 0.15, -0.1);
        // Traverse must not matter: the rule is the same in every direction.
        for yaw in [0.0, 1.4, -2.6, core::f32::consts::PI] {
            let bearing = |pitch: f32| can_bear(chassis, yaw, pitch);
            // Walk down until the gun gives up, then check the trigger gave up at the same angle.
            let mut refused = None;
            for step in 0..400 {
                let pitch = -0.005 * step as f32;
                let (_, elevation) =
                    barrel_angles(chassis, shooting::aim_ray(Vec3::ZERO, yaw, pitch).1);
                if elevation < -GUN_DEPRESSION {
                    refused = Some(pitch);
                    break;
                }
                assert!(bearing(pitch), "the trigger refused at {pitch}, inside the arc");
            }
            let refused = refused.expect("the barrel never ran out of depression");
            assert!(!bearing(refused), "the trigger fired at {refused}, past the arc");
        }
    }

    /// Looking *up* past the mount is not the trigger's business. The barrel stops following, but
    /// it is still pointing more or less where the driver is looking, and a weapon that went dead
    /// for looking at the sky would be a rule nobody could guess.
    #[test]
    fn looking_up_never_stops_the_trigger() {
        for pitch in [0.5, 1.0, 1.5] {
            assert!(can_bear(Quat::IDENTITY, 0.7, pitch), "the trigger refused a shot upwards");
        }
    }

    /// Looking up has to raise the muzzle. The sign of an elevation is invisible in a screenshot of
    /// a level gun and unmistakable in play.
    #[test]
    fn looking_up_raises_the_muzzle() {
        // At the resting traverse, so "forward" is the model's own nose at −X.
        let level = gun_pose(GUN_YAW, 0.0, 0.02).transform_point(GUN_MUZZLE);
        let raised = gun_pose(GUN_YAW, GUN_ELEVATION, 0.02).transform_point(GUN_MUZZLE);
        assert!(raised.y > level.y + 0.1, "the muzzle went from {level:?} to {raised:?}");
        // And the muzzle is out over the bonnet, which is what makes it a muzzle.
        assert!(level.x < GUN_MOUNT.x, "the barrel ends behind its own post: {level:?}");
    }
}
