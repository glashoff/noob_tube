//! A vehicle: a chassis on four spring struts.
//!
//! The wheels are **not** collision shapes. The whole vehicle is one dynamic box, and at four points
//! on it a ray is cast downwards; where it finds ground, a spring pushes the chassis up and a tyre
//! model resists sliding. That is the entire model, and it is what Havok gives Half-Life 2 and what
//! Halo's warthog runs on.
//!
//! It looks like a shortcut and is the opposite of one. Rolling cylinders catch on the seams between
//! triangles, climb steps they should bounce off, and need a much shorter timestep to stay stable —
//! all of which a rollback pays for four times over, once per replayed tick. Four rays cost four
//! rays, behave the same at any speed, and every property a designer wants to change is a number in
//! [`VehicleSpec`] rather than a solver setting.
//!
//! ### Where it sits in the network
//!
//! Unlike a crate, a vehicle is not a borderline case: the driver's input goes into it and the
//! result comes back out under the driver's own camera. Whoever is driving must predict it, and
//! everyone else interpolates it — the same split as a player, for the same reason. Until there is
//! a driver it has no input at all, so for now nobody predicts it and every client interpolates
//! what the server sends.
//!
//! That is why [`drive_vehicles`] takes a query filter, exactly as
//! [`step_players`](crate::simulation::step_players) does: the server steps every vehicle, and a
//! client will step only the one it is driving.

use avian3d::prelude::*;
use bevy::ecs::query::QueryFilter;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::physics::{Layer, WORLD_GRAVITY};
use crate::player::PlayerInput;

/// How many struts a vehicle has. Four, and the code says so once rather than in six places.
pub const WHEELS: usize = 4;
/// How many of them steer, and they are the first ones in [`VehicleSpec::mounts`].
pub const FRONT_WHEELS: usize = 2;

/// How far from upright a vehicle has to be before it counts as flipped, as the cosine of the
/// angle between its own up and the world's.
///
/// A fifth means about 78 degrees, which is well past anything the level can put it on: the ramp
/// leans it twelve degrees, and two wheels up a crate is under forty. Only a vehicle that is
/// genuinely on its side or its roof is beyond this, and there is no way back from either.
pub const FLIPPED_COSINE: f32 = 0.2;

/// How far ahead of itself a vehicle is allowed to predict a contact, in metres.
///
/// Avian predicts contacts before they happen — a *speculative* contact — and by default lets the
/// prediction reach as far as a body's velocity does. For anything that moves at walking pace that
/// is free accuracy. For a buggy at 24 m/s, which covers 38 cm in a tick, it is a metre-wide plane
/// of guesswork in front of the bumper, and the solver treats every contact surface as an infinite
/// plane: the vehicle brakes against a wall that is not there. Measured, hitting a 40 kg crate at
/// 23.4 m/s took **7 m/s** off a 1200 kg vehicle where the momentum it handed over accounts for
/// 0.7 — Avian's own documentation calls these ghost collisions and names a smaller margin as the
/// cure.
///
/// Ten centimetres is comfortably more than a tick of a walking pace and far less than a tick of a
/// driving one, which is exactly the split that matters. Bounding it takes the cost of hitting that
/// crate from 3.8 m/s down to **0.9**, against the 0.7 the momentum it hands over accounts for.
///
/// Swept CCD was the obvious partner for this and measurably does nothing: at margins of 0.10 m and
/// 0.02 m, with the sweep and without it, the same collision costs the same speed to a tenth. It
/// was never protecting anything here — the shortest thing in the level is a metre thick and the
/// vehicle covers 38 cm in a tick — so it is not switched on, and a sweep per body per replayed
/// tick is not a thing to pay for on the strength of the name.
///
/// **Not covered by a test, and not for want of trying.** The effect is flatly reproducible on a
/// running server — 7.2 m/s lost at an unbounded margin and at 0.50 m, 3.4 at 0.10 m and at 0.02 m,
/// across seven runs — and the same collision staged in the test world above costs the same to two
/// decimal places with the bound and without it. Something between the two differs and this comment
/// does not know what. Until it does, changing this number is a thing to measure live.
pub const SPECULATIVE_MARGIN: f32 = 0.1;

/// Which vehicle this is.
///
/// The numbers behind it are a constant both sides already have, not something sent per entity.
/// Tuning is shared knowledge in the same way the level's geometry is: client and server must agree
/// on it, and the way they agree is by being built from the same source. Replicating it would let
/// a vehicle exist whose handling nobody could have predicted from the build.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub enum VehicleKind {
    /// Four-wheeled off-roader. Open-topped, light, and happy to leave the ground.
    Buggy,
}

impl VehicleKind {
    pub const fn spec(self) -> &'static VehicleSpec {
        match self {
            VehicleKind::Buggy => &BUGGY,
        }
    }
}

/// Everything that makes one class of vehicle drive the way it does.
///
/// Deliberately not a component. It is a table entry, and a second vehicle is a second entry plus a
/// variant of [`VehicleKind`] — no new systems, no new replication.
#[derive(Clone, Debug)]
pub struct VehicleSpec {
    /// Half the nominal size of the chassis: the box the vehicle is *reckoned* to be.
    ///
    /// Not what it collides or is shot with any more — that is [`hull`](Self::hull) — but still
    /// what everything derives from: where the model is scaled to, how far the camera stands off,
    /// what counts as a teleport rather than a step.
    pub half_extents: Vec3,
    /// The shape a shot meets and a bumper hits: the model's own geometry, as convex hulls.
    ///
    /// A box was a bad stand-in for a Warthog and the bullet holes were what said so; sixteen
    /// hand-measured boxes were a better one and still not good enough. This is the third answer
    /// and the only one that is not a guess at a mesh that was sitting right there: the model is
    /// run through an approximate convex decomposition offline and the hulls are written out as
    /// numbers, by `tools/bake_collider`. See `shared/src/vehicle_shape.rs`, which is generated.
    ///
    /// Numbers rather than the model itself because the shape has to exist where the model does
    /// not: `/assets/` is gitignored, the server has no assets directory at all, and one of the
    /// two models may not be redistributed even if it did. Replacing a model means running the
    /// bake again, which is a command rather than an afternoon with a ruler.
    ///
    /// Fired at from twenty thousand directions and compared against the model's own triangles,
    /// this stops a shot a median of 0.7 cm from the bodywork against the sixteen boxes' 5.8, and
    /// 4 % of hits land more than 5 cm clear of a panel against their 54 %. Nothing passes through
    /// a vehicle it visibly hit any more; 2.9 % of shots used to, because the boxes had to leave
    /// the roll cage out and this does not.
    pub body: &'static [&'static [[f32; 3]]],
    /// Kilograms. The collider's density is derived from this so that the two cannot disagree.
    pub mass: f32,
    /// How far below the chassis centre the mass is placed.
    ///
    /// The single most effective knob against rolling over, and the reason a real off-roader puts
    /// its engine low. Raising it turns a vehicle that leans into one that tips.
    pub centre_of_mass_drop: f32,
    /// Where the struts are mounted, in chassis space. They all hang straight down the chassis's
    /// own −Y, so they must share a height — see [`VehicleSpec::ride_height`].
    pub mounts: [Vec3; WHEELS],
    /// How far a strut hangs when there is nothing under it.
    pub rest_length: f32,
    /// Wheel radius. Added to the strut's reach, because the wheel touches before its centre does.
    pub wheel_radius: f32,
    /// Wheel width. Only ever drawn, never collided with.
    pub wheel_width: f32,
    /// Newtons per metre of compression, per strut.
    pub stiffness: f32,
    /// Newton-seconds per metre, per strut. Resists the *rate* of compression, which is what stops
    /// a spring that would otherwise bounce for ever.
    pub damping: f32,
    /// How fast a loaded tyre kills sideways speed, as a rate per second.
    ///
    /// A rate rather than a fraction per tick on purpose: the tick rate is configurable, and a
    /// per-tick fraction would quietly change how the vehicle handles when it moved.
    pub grip: f32,
    /// The same, for the speed a free-rolling wheel loses. Small: a wheel is meant to roll.
    pub rolling_resistance: f32,
    /// How much sideways force a tyre can hold, as a multiple of the load on it.
    ///
    /// This is what turns [`grip`](Self::grip) from a wish into a limit. A rate alone would let a
    /// tyre take out any amount of sideways speed however lightly it was loaded, and at full lock a
    /// hard turn then scrubbed a vehicle to a standstill in three seconds — measured, before this
    /// existed. A real tyre can only pull about its own load sideways, so a wheel in the air holds
    /// nothing, a lightly loaded inside wheel holds little, and cornering too fast understeers
    /// instead of stopping dead.
    pub friction: f32,
    /// Newtons the engine puts through the tyres at full throttle, across all four.
    pub drive_force: f32,
    /// Newtons the brakes can take out, across all four.
    pub brake_force: f32,
    /// How far the front wheels turn at full lock, in radians.
    pub max_steer: f32,
    /// How fast they get there, in radians per second. A steering wheel is not a switch, and a
    /// front axle that snapped to full lock in one tick would flip the vehicle on the spot.
    pub steer_rate: f32,
    /// Metres per second past which the engine stops pushing.
    ///
    /// Not a cap on the speed — a hill will still take it faster. It is where the drive force
    /// tapers to nothing, which is what a gearbox and drag do in a real vehicle and what stops a
    /// constant force from accelerating for ever.
    pub top_speed: f32,
    /// Seconds a vehicle lies past [`FLIPPED_COSINE`] before it is helped back on to its wheels.
    ///
    /// Not zero, and not for politeness: a vehicle in the middle of a barrel roll is past the
    /// threshold for a fraction of a second on its way to landing on its wheels by itself, and
    /// righting it then would take the roll away from the driver who earned it.
    pub righting_delay: f32,
    /// Radians per second squared, per radian of lean, once the delay has passed.
    ///
    /// An angular *acceleration* rather than a torque, so the number means the same thing for a
    /// buggy and for a truck: Avian divides a torque by the inertia this vehicle happens to have,
    /// and that inertia is derived from the chassis box.
    pub righting_stiffness: f32,
    /// Per second, against the spin the righting itself produces. Without it the vehicle rolls
    /// past upright and comes back, which is a vehicle rocking on its roof rather than getting up.
    pub righting_damping: f32,
    /// Metres per second squared upward while it is getting up, against gravity.
    ///
    /// Not decoration. A vehicle on its roof is lying on a face, and turning it means lifting its
    /// mass over the edge it rests on — 10.6 kN·m for this one, which is more than any torque
    /// gentle enough to look like a vehicle rather than a catapult. Taking most of its weight off
    /// the ground first drops that to a third and lets the rest be a nudge. Below gravity, so it
    /// never lifts off; what it does is make the vehicle light on its edge.
    pub righting_lift: f32,
}

impl VehicleSpec {
    /// Kilograms per cubic metre, so that [`ColliderDensity`] and [`mass`](Self::mass) agree by
    /// construction. Avian derives both mass and inertia from the collider and its density, so
    /// setting the mass directly would leave the inertia describing a different vehicle.
    ///
    /// Divided by the volume the *collider* reports rather than by one worked out here. Avian adds
    /// a compound's parts up, and a decomposition's parts do touch and overlap a little at their
    /// seams; asking the shape what it weighs at unit density is the only sum that is guaranteed
    /// to be the same sum Avian will do. A test weighs the result.
    pub fn density(&self) -> f32 {
        self.mass / self.collider().mass_properties(1.0).mass
    }

    /// The shape the chassis is: one convex hull per part of the baked decomposition.
    ///
    /// Rebuilt on demand rather than cached. It is wanted once per vehicle spawned, and a
    /// `Collider` is neither `const` nor cheap to keep in a static.
    pub fn collider(&self) -> Collider {
        Collider::compound(
            self.body
                .iter()
                .map(|hull| {
                    (
                        Vec3::ZERO,
                        Quat::IDENTITY,
                        Collider::convex_hull(
                            hull.iter().copied().map(Vec3::from_array).collect(),
                        )
                        .expect("a baked part is the hull of at least four points"),
                    )
                })
                .collect(),
        )
    }

    /// How far the struts compress under the vehicle's own weight, on level ground.
    ///
    /// Four springs share the weight, so each carries `mg/4` and gives way by that over its
    /// stiffness. This is what makes the spring constants checkable rather than guessed: pick the
    /// ride height you want and this says what stiffness produces it.
    pub fn static_compression(&self) -> f32 {
        self.mass * -WORLD_GRAVITY / (WHEELS as f32 * self.stiffness)
    }

    /// How high the chassis centre rides once it has settled on level ground.
    pub fn ride_height(&self) -> f32 {
        self.rest_length + self.wheel_radius - self.static_compression() - self.mounts[0].y
    }

    /// How far a strut can reach before the wheel is off the ground.
    fn reach(&self) -> f32 {
        self.rest_length + self.wheel_radius
    }
}

/// The buggy.
///
/// 1200 kg on struts stiff enough to settle about 18 cm down, damped to a little under half of
/// critical — enough that landing from a jump compresses visibly and comes back once rather than
/// wallowing. The mass sits 35 cm below the middle of the body, which is what keeps it on its
/// wheels through a fast corner.
pub const BUGGY: VehicleSpec = VehicleSpec {
    half_extents: Vec3::new(0.9, 0.4, 1.9),
    body: crate::vehicle_shape::BUGGY_BODY,
    mass: 1200.0,
    centre_of_mass_drop: 0.35,
    // Just inside the body, at its underside. Front is −Z, as everywhere else in this game.
    mounts: [
        Vec3::new(-0.85, -0.4, -1.35),
        Vec3::new(0.85, -0.4, -1.35),
        Vec3::new(-0.85, -0.4, 1.35),
        Vec3::new(0.85, -0.4, 1.35),
    ],
    rest_length: 0.45,
    wheel_radius: 0.4,
    wheel_width: 0.3,
    stiffness: 16_000.0,
    damping: 2_000.0,
    grip: 16.0,
    rolling_resistance: 0.15,
    // A shade over 1 g of cornering, which is a good road tyre and a generous off-road one.
    friction: 1.2,
    // 12 kN through 1200 kg is 10 m/s²: nought to twenty in two seconds, which is brisk rather
    // than silly. Brakes stronger than the engine, as on anything that has to stop as well as go.
    drive_force: 12_000.0,
    brake_force: 20_000.0,
    // About 31 degrees of lock, reached in a fifth of a second.
    max_steer: 0.55,
    steer_rate: 3.0,
    top_speed: 25.0,
    // Long enough to let a roll finish on its own, short enough that a driver on their roof does
    // not reach for the menu. Upside down is pi radians of lean, so the stiffness starts it at
    // about 13 rad/s squared and the damping settles it at around four radians a second.
    righting_delay: 1.5,
    righting_stiffness: 8.0,
    righting_damping: 4.0,
    // Six tenths of a g. Enough to make the roll cheap, not enough to leave the ground.
    righting_lift: 6.0,
};

/// On a player: they are in a vehicle rather than on their feet.
///
/// Replicated, because everyone needs it. The driver's own client switches the camera to the
/// vehicle; every other client stops drawing that player, since they are inside the bodywork.
/// [`step_players`](crate::simulation::step_players) skips them, which is the whole of "you cannot
/// walk while driving".
///
/// A marker rather than the vehicle's entity, because an entity reference across the wire needs
/// mapping, and that is a whole mechanism to buy something [`Driven`] gives for a byte.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Driving;

/// On a vehicle: whose seat is taken, by [`Player::peer`](crate::player::Player::peer).
///
/// The other half of [`Driving`], and it exists because the shortcut it replaces stopped being
/// true. A client used to know which vehicle it was driving by elimination — it was the only one it
/// predicted — and then parked vehicles started being predicted too, so that a driver would not ram
/// an immovable copy of one. From that moment "the vehicle I predict" and "the vehicle I drive" are
/// two different sets.
///
/// It carries the peer number rather than being a bare marker, and that is what makes the answer
/// hold when [`predict_vehicles`](crate::tuning::NetConfig::predict_vehicles) is off: with nothing
/// predicted, "the one I predict and that is driven" identifies nothing at all. A client compares
/// this against the `Player` on its own predicted body and needs no other machinery.
///
/// Still not an entity reference. An entity across the wire needs mapping, and this buys the same
/// answer for eight bytes.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Driven(pub u64);

/// What the driver is asking for, this tick.
///
/// A component rather than something worked out inside the driving step, because the two sides
/// arrive at it differently and the step must not care: the server looks up whoever is in the seat,
/// a client uses its own input and knows there is only one vehicle it could possibly be driving.
/// Everything after this point is identical on both.
///
/// It is derived fresh from the input every tick, so a rollback reproduces it exactly; there is no
/// state in here that a replay could get wrong. `steer` is the exception and deliberately so — it
/// is the wheels' *current* angle, eased toward what is being asked for, and easing is a thing with
/// memory. Replay reproduces it because it starts from the same angle and sees the same inputs.
///
/// **Replicated**, and that is what lets a client predict a vehicle it is not driving. Its own it
/// derives from its own input; anyone else's it cannot, because that input belongs to a peer it
/// never hears from — but the *result* of that input arrives here every update, and holding the
/// last one for the length of the prediction window is a far better guess than a frozen box in the
/// past. That is the whole reason [`wanted_steer`](Self::wanted_steer) travels alongside the angle:
/// with the driver's intent in hand, a client can keep easing the wheels the way the server is
/// easing them, rather than freezing them mid-turn.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Controls {
    /// −1 hard on the brakes, +1 full throttle.
    pub throttle: f32,
    /// Where the front wheels actually point, in radians. Negative is left.
    pub steer: f32,
    /// Everything locked, for stopping and for turning without rolling.
    pub handbrake: bool,
    /// Where the driver is asking the front wheels to point, which is where `steer` is heading.
    ///
    /// The intent rather than the state, and the two are different for up to a fifth of a second at
    /// [`steer_rate`](VehicleSpec::steer_rate). A peer holding only the angle would freeze a turn
    /// halfway through it; a peer holding the intent finishes the turn exactly as the server does,
    /// and is wrong only from the moment the driver actually changes their mind.
    pub wanted_steer: f32,
}

impl Controls {
    /// Advances the controls by one tick of a driver's intent.
    pub fn apply_input(&mut self, spec: &VehicleSpec, input: &PlayerInput, dt: f32) {
        self.throttle = (input.forward as i32 - input.backward as i32) as f32;
        self.handbrake = input.jump;
        self.wanted_steer = (input.right as i32 - input.left as i32) as f32 * spec.max_steer;
        self.ease(spec, dt);
    }

    /// Advances the wheels toward what was last asked for, with no new input to go on.
    ///
    /// What a peer runs for a vehicle it predicts but does not drive. It is the same easing
    /// `apply_input` ends with, so a client that keeps calling this reproduces the server's steering
    /// exactly for as long as the driver holds the wheel where it is.
    pub fn ease(&mut self, spec: &VehicleSpec, dt: f32) {
        let step = spec.steer_rate * dt;
        self.steer = if (self.wanted_steer - self.steer).abs() <= step {
            self.wanted_steer
        } else {
            self.steer + (self.wanted_steer - self.steer).signum() * step
        };
    }
}

/// Where one wheel ended up this tick.
///
/// Everything here is derived, every tick, from the chassis pose and the ground under it. None of it
/// is replicated: a wheel is a picture, and a peer that has the chassis pose can work out the same
/// picture for itself with one ray. Sending four wheel states per vehicle per update to save four
/// rays per frame would be the wrong trade twice over.
#[derive(Clone, Copy, Debug, Default, Reflect)]
pub struct Wheel {
    /// Where the strut is mounted, in world space.
    pub mount: Vec3,
    /// The wheel's centre, in world space: as far down the strut as the ground allows.
    pub centre: Vec3,
    /// Where the tyre meets the ground. Meaningless unless `grounded`.
    pub contact: Vec3,
    /// Which way that ground faces. Meaningless unless `grounded`.
    pub normal: Vec3,
    /// How far the strut is compressed from its rest length, in metres. Zero in the air.
    pub compression: f32,
    /// Whether the wheel found ground within the strut's reach.
    pub grounded: bool,
}

/// What the four struts found this tick.
#[derive(Component, Clone, Copy, Debug, Default, Reflect)]
#[reflect(Component)]
pub struct Wheels(pub [Wheel; WHEELS]);

/// How long this vehicle has been lying on its side or its roof.
///
/// State, and the only state in the vehicle besides the steering angle. It cannot be derived from
/// the pose because it is a *duration*, and the whole point of it is that a vehicle mid-roll and a
/// vehicle stuck on its roof look identical for the first half second.
///
/// Not replicated. A client that predicts a vehicle counts for itself and reaches the same answer
/// from the same poses, and a client that only interpolates one never asks — it is shown the
/// result of the server's righting as ordinary movement, which is what it is.
#[derive(Component, Clone, Copy, Debug, Default, Reflect)]
#[reflect(Component)]
pub struct Righting {
    /// Seconds past [`FLIPPED_COSINE`], reset to zero the moment it is back within it.
    pub flipped_for: f32,
}

/// Everything a vehicle needs to exist as a physical body.
///
/// The pose is `Position`/`Rotation` rather than a `Transform`, because those are what Avian and
/// lightyear both treat as the authority — a body spawned with only a transform sits at the origin
/// until a sync system has run, which for something spawned in `Startup` is after the first tick.
pub fn vehicle_body(kind: VehicleKind, at: Vec3, facing: Quat) -> impl Bundle {
    let spec = kind.spec();
    (
        kind,
        Wheels::default(),
        Controls::default(),
        Righting::default(),
        RigidBody::Dynamic,
        spec.collider(),
        ColliderDensity(spec.density()),
        CenterOfMass(Vec3::NEG_Y * spec.centre_of_mass_drop),
        // Not on the level layer: a vehicle is not terrain, and the movement queries deliberately
        // do not see it. Walking on one is its own piece of work.
        CollisionLayers::new(Layer::Body, LayerMask::ALL),
        SpeculativeMargin(SPECULATIVE_MARGIN),
        // What stops the vehicle braking against contacts it has not reached yet. See
        // [`SPECULATIVE_MARGIN`], including why the swept CCD that would normally accompany it is
        // not here.
        Position(at),
        Rotation(facing),
    )
}

/// Where each strut's wheel is, given where the chassis is.
///
/// A free function, and pure apart from the collider trees it reads, because two very different
/// callers need exactly this: the simulation, to know where to push, and a client drawing a vehicle
/// it does not simulate, to know where to put the wheel meshes. One of them must not apply forces,
/// so the two halves are separated here rather than by a flag.
pub fn probe_wheels(
    spec: &VehicleSpec,
    position: Vec3,
    rotation: Quat,
    space: &SpatialQuery,
    chassis: Entity,
) -> [Wheel; WHEELS] {
    // The chassis itself has a collider, and the struts are mounted on its underside — without this
    // every ray would immediately hit the body it starts inside.
    let filter = SpatialQueryFilter::from_excluded_entities([chassis]);
    let down = rotation * Vec3::NEG_Y;
    let direction = Dir3::new(down).unwrap_or(Dir3::NEG_Y);
    let reach = spec.reach();

    spec.mounts.map(|local| {
        let mount = position + rotation * local;
        let hit = space.cast_ray(mount, direction, reach, true, &filter);
        match hit {
            Some(hit) => Wheel {
                mount,
                centre: mount + down * (hit.distance - spec.wheel_radius),
                contact: mount + down * hit.distance,
                normal: hit.normal,
                compression: reach - hit.distance,
                grounded: true,
            },
            None => Wheel {
                mount,
                centre: mount + down * spec.rest_length,
                contact: Vec3::ZERO,
                normal: Vec3::ZERO,
                compression: 0.0,
                grounded: false,
            },
        }
    })
}

/// FixedUpdate: holds every matching vehicle up on its springs, and does what the driver asked.
///
/// Runs before the solver step in `FixedPostUpdate`, which is what consumes the forces; both are
/// inside `FixedMain`, so a rollback replays this once per replayed tick exactly as it replays
/// movement. It reads nothing but its arguments and the collider trees, which is what makes that
/// replay reproduce the same result.
///
/// **Steering is not a torque.** Turning the front wheels only changes which way their grip points;
/// the sideways force that grip produces is what swings the vehicle round, at the contact patch,
/// through the length of the wheelbase. That is why a vehicle with a wheel in the air understeers
/// and one on ice does not turn at all, without any of it being written down anywhere.
pub fn drive_vehicles<F: QueryFilter + 'static>(
    space: SpatialQuery,
    time: Res<Time<Fixed>>,
    mut vehicles: Query<(Entity, &VehicleKind, &Controls, &mut Wheels, Forces), F>,
) {
    let dt = time.delta_secs();

    for (entity, kind, controls, mut wheels, mut body) in vehicles.iter_mut() {
        let spec = kind.spec();
        let position = body.position().0;
        let rotation = body.rotation().0;
        let found = probe_wheels(spec, position, rotation, &space, entity);

        // The strut's own axis. A spring resists being compressed along its length, whatever the
        // ground beneath it is doing, so this is the direction the force acts in — not the ground's
        // normal, which only decides how much of it the ground can return.
        let up = rotation * Vec3::Y;
        let share = spec.mass / WHEELS as f32;
        // A rate turned into this tick's fraction. `1 − e^{−kt}` rather than `k·t` so that halving
        // the tick rate does not change how the vehicle handles, and so that a large rate can never
        // remove more than all of the speed and start pushing the other way.
        let grip = 1.0 - (-spec.grip * dt).exp();
        let drag = 1.0 - (-spec.rolling_resistance * dt).exp();
        // Only the front wheels turn, and they turn the other way from the sign convention: a
        // positive `steer` is a right turn, and rotating −Z about +Y by a positive angle goes left.
        let steered = Quat::from_rotation_y(-controls.steer);

        for (index, wheel) in found.iter().enumerate() {
            if !wheel.grounded {
                continue;
            }
            let velocity = body.velocity_at_point(wheel.contact);
            // Spring minus damper, and never negative: a strut can push the chassis away from the
            // ground, but it cannot pull it back down. Letting it go negative is how a car ends up
            // sucked onto the road and unable to leave a ramp.
            let load =
                (spec.stiffness * wheel.compression - spec.damping * velocity.dot(up)).max(0.0);
            body.apply_force_at_point(up * load, wheel.contact);

            // Which way this tyre is pointing. Flattened onto the ground first, or on a slope some
            // of the grip would act as lift.
            let turn = if index < FRONT_WHEELS { steered } else { Quat::IDENTITY };
            let facing = rotation * turn;
            let forward = (facing * Vec3::NEG_Z).reject_from(wheel.normal).normalize_or_zero();
            let right = (facing * Vec3::X).reject_from(wheel.normal).normalize_or_zero();

            // Sideways grip, at the contact patch rather than at the centre of mass. That is what
            // makes the body lean into a corner and dip under braking — the same force through a
            // longer lever arm — and it is also what lets it roll over if the mass sits too high.
            //
            // Capped by what this tyre is actually carrying. `load` is the suspension force that
            // has just been applied through it, so an unloaded wheel grips nothing and the inside
            // wheels of a fast corner grip less than the outside ones — which is understeer, and it
            // arrives without being written down anywhere.
            let wanted = -velocity.dot(right) * grip * share;
            let strongest = spec.friction * load * dt;
            body.apply_linear_impulse_at_point(
                right * wanted.clamp(-strongest, strongest),
                wheel.contact,
            );

            let along = velocity.dot(forward);
            // Braking covers three things that are the same act: the handbrake, asking to go
            // backwards while still going forwards, and asking to go forwards while still rolling
            // back. The half a metre a second is the dead band that lets the third become reverse
            // rather than an eternal fight against a stopped vehicle.
            let braking = controls.handbrake
                || (controls.throttle < 0.0 && along > 0.5)
                || (controls.throttle > 0.0 && along < -0.5);

            if braking {
                // At most what the brakes can take out this tick, and never more than the speed
                // there is — overshooting would drive the vehicle backwards out of a stop.
                let strongest = spec.brake_force / WHEELS as f32 * dt;
                let needed = along.abs() * share;
                body.apply_linear_impulse_at_point(
                    forward * (-along.signum() * needed.min(strongest)),
                    wheel.contact,
                );
            } else if controls.throttle != 0.0 {
                // Tapered to nothing at the top speed. Not a cap — a hill will still take it
                // faster — but it is what stops a constant force accelerating for ever, which is
                // what a gearbox and the air do on a real vehicle.
                let taper = (1.0 - along.abs() / spec.top_speed).clamp(0.0, 1.0);
                let force = spec.drive_force / WHEELS as f32 * controls.throttle * taper;
                body.apply_force_at_point(forward * force, wheel.contact);
            } else {
                // Coasting: a wheel is meant to roll, and only just resists it.
                body.apply_linear_impulse_at_point(
                    forward * (-along * drag * share),
                    wheel.contact,
                );
            }
        }

        wheels.0 = found;
    }
}

/// FixedUpdate: puts a vehicle that has ended up on its roof back on its wheels.
///
/// A vehicle on its side is not a hard problem to drive out of, it is an impossible one: the wheels
/// find no ground, so the whole model — spring, damper, tyre — has nothing to act through, and the
/// only thing still touching the world is a box that slides. Without this the buggy is lost the
/// first time somebody takes the ramp badly, and the round has one fewer vehicle in it.
///
/// The correction is an angular acceleration toward upright with a damper against itself, which is
/// the same spring-and-damper shape as a strut and behaves the same way: it accelerates hardest
/// when the lean is worst, and it settles rather than rocking. Deliberately *not* a snap to an
/// upright pose — a teleport is a rollback's worst case, and a client and a server that snap on
/// slightly different ticks disagree by the whole of the flip.
///
/// It says nothing about yaw. [`Quat::from_rotation_arc`] gives the shortest turn that takes one
/// direction to another, so a vehicle that lands facing a wall is stood up still facing the wall.
/// Which way it points is the driver's business.
pub fn right_flipped_vehicles<F: QueryFilter + 'static>(
    time: Res<Time<Fixed>>,
    mut vehicles: Query<(&VehicleKind, &mut Righting, Forces), F>,
) {
    let dt = time.delta_secs();

    for (kind, mut righting, mut body) in vehicles.iter_mut() {
        let spec = kind.spec();
        let up = body.rotation().0 * Vec3::Y;

        if up.y > FLIPPED_COSINE {
            righting.flipped_for = 0.0;
            continue;
        }
        righting.flipped_for += dt;
        if righting.flipped_for < spec.righting_delay {
            continue;
        }

        // The shortest turn from where its roof points to where the sky is. Exactly upside down is
        // the degenerate case — every axis is equally short — and glam already picks one there
        // rather than returning something with a zero axis.
        let (axis, angle) = Quat::from_rotation_arc(up, Vec3::Y).to_axis_angle();
        let spin = body.angular_velocity();
        body.apply_angular_acceleration(
            axis * angle * spec.righting_stiffness - spin * spec.righting_damping,
        );
        // And most of its weight, so the turn has an edge to pivot on rather than a face to drag.
        body.apply_linear_acceleration(Vec3::Y * spec.righting_lift);
    }
}

#[cfg(test)]
mod tests {

    /// The shape has to *be* something before anything else about it is worth asking.
    #[test]
    fn the_body_is_a_run_of_convex_parts() {
        let spec = BUGGY;
        assert!(!spec.body.is_empty(), "a vehicle with no body has nothing to be shot at");
        for (index, part) in spec.body.iter().enumerate() {
            assert!(
                part.len() >= 4,
                "part {index} has {} points, which is not a solid",
                part.len(),
            );
            assert!(
                Collider::convex_hull(part.iter().copied().map(Vec3::from_array).collect())
                    .is_some(),
                "part {index} does not make a hull, so Avian would drop it silently",
            );
        }
    }

    /// The bake is only the right shape if it was placed the way the model is placed.
    ///
    /// The client turns, scales and lifts the *visible* model by numbers of its own; the bake did
    /// the same by numbers of its own; and nothing but this holds the two together. Get it wrong
    /// and every shot lands where the bodywork is not — the exact bug, with a subtler cause.
    #[test]
    fn the_body_sits_where_the_model_is_drawn() {
        let spec = BUGGY;
        let (mut low, mut high) = (Vec3::MAX, Vec3::MIN);
        for point in spec.body.iter().flat_map(|part| part.iter()) {
            let point = Vec3::from_array(*point);
            low = low.min(point);
            high = high.max(point);
        }
        // The length is what the model was scaled *to*, so it has to come back out exactly.
        assert!(
            (high.z - low.z - spec.half_extents.z * 2.0).abs() < 0.05,
            "the body is {:.3} m long but the vehicle is reckoned to be {:.3}",
            high.z - low.z,
            spec.half_extents.z * 2.0,
        );
        // And it has to straddle the chassis origin rather than sit above or in front of it.
        assert!(
            low.z < -1.5 && high.z > 1.5 && low.x < -0.5 && high.x > 0.5,
            "the body runs x {:.2}..{:.2}, z {:.2}..{:.2}, which is not centred on the chassis",
            low.x,
            high.x,
            low.z,
            high.z,
        );
        // The lift is what puts the underside just under the nominal box and the cage well above
        // it. A sign error in it would show up here as a body that floats or is buried.
        assert!(
            (-0.7..-0.3).contains(&low.y),
            "the underside is at y {low:?}, which is not where a sill is",
        );
        assert!(high.y > 0.5, "nothing reaches above y {high:?}, so the roll cage is missing");
    }

    /// The shape has to weigh what the spec says, or the density is a number that means nothing.
    ///
    /// This is the whole of why `density` divides by the volume the collider reports rather than
    /// by the nominal box: Avian derives the mass from the shape, and a shape that is a third the
    /// volume of the box would quietly make a 1200 kg buggy weigh 700.
    #[test]
    fn the_body_weighs_what_the_spec_says() {
        let spec = BUGGY;
        let mass = spec.collider().mass_properties(spec.density()).mass;
        assert!(
            (mass - spec.mass).abs() < spec.mass * 0.01,
            "the spec says {} kg, the shape weighs {mass} kg",
            spec.mass,
        );
    }

    /// The body is meant to be *tighter* than the box it replaced. If a change ever loosens it
    /// back to a box, the bullet holes start hanging in the air again, and quietly.
    #[test]
    fn the_body_hugs_the_model_more_closely_than_a_box_would() {
        let spec = BUGGY;
        let box_volume = 8.0 * spec.half_extents.x * spec.half_extents.y * spec.half_extents.z;
        let volume = spec.collider().mass_properties(1.0).mass;
        assert!(
            volume < box_volume * 0.75,
            "the body takes up {volume:.2} m³ against the box's {box_volume:.2} m³, no better",
        );
        // And it reaches past the nominal box downwards, which is the underbody, and upwards,
        // which is the roll cage. A shape trimmed to the box misses both.
        let reach = spec
            .body
            .iter()
            .flat_map(|part| part.iter())
            .fold((0.0f32, 0.0f32), |(low, high), point| (low.min(point[1]), high.max(point[1])));
        assert!(
            reach.0 < -spec.half_extents.y && reach.1 > spec.half_extents.y,
            "the body runs y {:.2}..{:.2}, inside the nominal box's ±{:.2}",
            reach.0,
            reach.1,
            spec.half_extents.y,
        );
    }

    use super::*;
    use crate::physics::test_support::floor_app;
    use bevy::time::TimeUpdateStrategy;
    use core::time::Duration;

    const HZ: f64 = 64.0;

    /// A floor, a fixed tick, and the suspension step — the smallest world a vehicle can stand in.
    ///
    /// The clock is driven by hand. Left to the wall clock a test would take as many real seconds
    /// as it simulates, and would give a different answer on a loaded machine.
    fn driving_app() -> App {
        let mut app = floor_app();
        app.insert_resource(Time::<Fixed>::from_hz(HZ));
        app.insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_secs_f64(
            1.0 / HZ,
        )));
        app.add_systems(FixedUpdate, (drive_vehicles::<()>, right_flipped_vehicles::<()>));
        app
    }

    fn park(app: &mut App, at: Vec3) -> Entity {
        let car = app
            .world_mut()
            .spawn(vehicle_body(VehicleKind::Buggy, at, Quat::IDENTITY))
            .id();
        // One update so the collider is sized and its mass properties computed before anything
        // asks what the vehicle weighs.
        app.update();
        car
    }

    /// Puts the driver's hands on it. `steer` is the wheel angle, not the input.
    fn hands(app: &mut App, car: Entity, throttle: f32, steer: f32) {
        let mut controls = app.world_mut().get_mut::<Controls>(car).expect("controls");
        controls.throttle = throttle;
        controls.steer = steer * VehicleKind::Buggy.spec().max_steer;
    }

    fn facing(app: &App, car: Entity) -> Vec3 {
        app.world().get::<Rotation>(car).expect("a rotation").0 * Vec3::NEG_Z
    }

    fn run(app: &mut App, seconds: f32) {
        for _ in 0..(seconds * HZ as f32) as usize {
            app.update();
        }
    }

    fn pose(app: &App, car: Entity) -> Vec3 {
        app.world().get::<Position>(car).expect("a position").0
    }

    fn speed(app: &App, car: Entity) -> Vec3 {
        app.world().get::<LinearVelocity>(car).expect("a velocity").0
    }

    /// Puts a vehicle down at the given attitude and lets it settle.
    fn park_facing(app: &mut App, at: Vec3, facing: Quat) -> Entity {
        let car = app
            .world_mut()
            .spawn(vehicle_body(VehicleKind::Buggy, at, facing))
            .id();
        app.update();
        car
    }

    /// How far its own up is from the world's, in degrees.
    fn lean(app: &App, car: Entity) -> f32 {
        let up = app.world().get::<Rotation>(car).expect("a rotation").0 * Vec3::Y;
        up.y.clamp(-1.0, 1.0).acos().to_degrees()
    }

    /// The whole point: a vehicle on its roof has no way back on its own, because the wheels find
    /// no ground and the entire model acts through the wheels.
    #[test]
    fn on_its_roof_it_gets_itself_back_up() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park_facing(
            &mut app,
            Vec3::Y * (spec.ride_height() + 0.5),
            Quat::from_rotation_z(core::f32::consts::PI),
        );
        run(&mut app, 6.0);

        assert!(lean(&app, car) < 10.0, "still leaning {:.1} degrees over", lean(&app, car));
        let y = pose(&app, car).y;
        assert!(
            (y - spec.ride_height()).abs() < 0.1,
            "back up but sitting at {y:.3} m rather than {:.3} m",
            spec.ride_height()
        );
    }

    /// And on its side, which is what actually happens: a vehicle rarely lands squarely upside
    /// down, it drops onto a flank and stays there.
    #[test]
    fn on_its_side_it_gets_itself_back_up() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park_facing(
            &mut app,
            Vec3::Y * (spec.ride_height() + 0.5),
            Quat::from_rotation_z(core::f32::consts::FRAC_PI_2),
        );
        run(&mut app, 6.0);

        assert!(lean(&app, car) < 10.0, "still leaning {:.1} degrees over", lean(&app, car));
    }

    /// It must not touch a vehicle that is merely tilted. The ramp leans it twelve degrees and a
    /// wheel up a kerb rather more; righting either would be a hand on the wheel nobody asked for.
    #[test]
    fn a_leaning_vehicle_is_left_alone() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let tilt = Quat::from_rotation_z(0.6);
        let car = park_facing(&mut app, tilt * Vec3::Y * spec.ride_height(), tilt);
        app.update();

        let before = lean(&app, car);
        run(&mut app, 3.0);
        let righting = app.world().get::<Righting>(car).expect("a righting");
        assert_eq!(righting.flipped_for, 0.0, "a {before:.0}-degree lean counted as flipped");
    }

    /// The delay is the difference between helping and interfering: a vehicle that is upside down
    /// for a moment on its way through a roll has to be left to finish it.
    #[test]
    fn it_waits_before_it_intervenes() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park_facing(
            &mut app,
            Vec3::Y * (spec.ride_height() + 0.5),
            Quat::from_rotation_z(core::f32::consts::PI),
        );
        run(&mut app, spec.righting_delay - 0.2);

        assert!(
            lean(&app, car) > 90.0,
            "it started standing itself up after {:.1} s, before the {:.1} s delay",
            spec.righting_delay - 0.2,
            spec.righting_delay,
        );
    }

    /// [`VehicleSpec::ride_height`] assumes every strut hangs from the same height, which is what
    /// lets one number describe where the chassis settles. A spec that broke it would make every
    /// other test in here quietly meaningless.
    #[test]
    fn every_strut_hangs_from_the_same_height() {
        let spec = VehicleKind::Buggy.spec();
        for mount in spec.mounts {
            assert_eq!(mount.y, spec.mounts[0].y, "a strut is mounted at a different height");
        }
    }

    /// The arithmetic behind the spring constants, checked against the simulation rather than
    /// against itself: park the vehicle at the height the springs predict and it should stay there.
    #[test]
    fn it_stays_at_the_ride_height_its_springs_predict() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 2.0);

        let y = pose(&app, car).y;
        assert!(
            (y - spec.ride_height()).abs() < 0.02,
            "settled at {y:.3} m, where the springs say {:.3} m",
            spec.ride_height()
        );
    }

    /// Dropped from above, it must come down to the same height and stop there. This is the damper:
    /// without it the springs are lossless and the vehicle bounces for ever.
    #[test]
    fn dropped_from_a_height_it_settles_rather_than_bouncing() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * (spec.ride_height() + 2.0));
        run(&mut app, 4.0);

        let y = pose(&app, car).y;
        assert!(
            (y - spec.ride_height()).abs() < 0.05,
            "came to rest at {y:.3} m, not at {:.3} m",
            spec.ride_height()
        );
        assert!(
            speed(&app, car).length() < 0.05,
            "still moving at {:.3} m/s after four seconds",
            speed(&app, car).length()
        );
    }

    /// It must not sink into the floor. The chassis has a collider of its own, so a suspension that
    /// did nothing would still be caught by the solver — which is exactly the failure that looks
    /// like it works.
    #[test]
    fn it_rides_clear_of_the_ground_on_its_wheels() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * (spec.ride_height() + 0.5));
        run(&mut app, 3.0);

        let underside = pose(&app, car).y - spec.half_extents.y;
        assert!(
            underside > 0.3,
            "the body is {underside:.3} m off the ground: it is resting on itself, not on its wheels"
        );
    }

    /// In the air nothing is pushing, and nothing must pretend to be.
    #[test]
    fn wheels_off_the_ground_find_nothing() {
        let mut app = driving_app();
        let car = park(&mut app, Vec3::Y * 20.0);
        app.update();

        let wheels = app.world().get::<Wheels>(car).expect("wheels").0;
        for wheel in wheels {
            assert!(!wheel.grounded, "a wheel twenty metres up found ground");
            assert_eq!(wheel.compression, 0.0);
        }
    }

    /// On level ground all four struts must carry the same share. An asymmetry here would show up
    /// as a vehicle that pulls to one side for no reason a driver could see.
    #[test]
    fn all_four_struts_take_the_same_load() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 2.0);

        let wheels = app.world().get::<Wheels>(car).expect("wheels").0;
        let expected = spec.static_compression();
        for wheel in wheels {
            assert!(wheel.grounded, "a wheel lost the ground while parked");
            assert!(
                (wheel.compression - expected).abs() < 0.01,
                "a strut sits at {:.3} m where the others sit at {expected:.3} m",
                wheel.compression
            );
        }
    }

    /// A tyre resists sideways much harder than it resists rolling. That difference *is* the tyre:
    /// without it a vehicle is a sledge, and steering it would do nothing but change which way it
    /// points while it kept going the way it was.
    #[test]
    fn it_slides_sideways_far_less_easily_than_it_rolls() {
        let spec = VehicleKind::Buggy.spec();
        let shove = |push: Vec3| {
            let mut app = driving_app();
            let car = park(&mut app, Vec3::Y * spec.ride_height());
            run(&mut app, 0.5);
            app.world_mut().get_mut::<LinearVelocity>(car).expect("a velocity").0 = push;
            run(&mut app, 1.0);
            speed(&app, car).length()
        };

        let sideways = shove(Vec3::X * 8.0);
        let rolling = shove(Vec3::NEG_Z * 8.0);
        assert!(
            sideways < rolling * 0.5,
            "sideways it still does {sideways:.2} m/s against {rolling:.2} m/s rolling: the tyres \
             are not gripping"
        );
        assert!(rolling > 6.0, "it coasts to {rolling:.2} m/s from 8: the wheels are dragging");
    }

    /// Full throttle has to move it, forwards, and not sideways or into the ground.
    #[test]
    fn the_throttle_drives_it_forward() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 0.5);
        hands(&mut app, car, 1.0, 0.0);
        run(&mut app, 2.0);

        let travelled = pose(&app, car);
        assert!(travelled.z < -8.0, "went {:.1} m in two seconds", -travelled.z);
        assert!(travelled.x.abs() < 0.5, "wandered {:.2} m sideways", travelled.x);
        assert!(
            (travelled.y - spec.ride_height()).abs() < 0.1,
            "left the ground, or dug into it: {:.2} m",
            travelled.y
        );
    }

    /// And the brakes have to stop it — without hauling it backwards through zero, which is what an
    /// unbounded braking force does on the tick the speed runs out.
    #[test]
    fn the_brakes_stop_it_without_reversing_it() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 0.5);
        hands(&mut app, car, 1.0, 0.0);
        run(&mut app, 2.0);
        let rolling = speed(&app, car).length();
        assert!(rolling > 5.0, "never got going: {rolling:.1} m/s");

        hands(&mut app, car, 0.0, 0.0);
        app.world_mut().get_mut::<Controls>(car).unwrap().handbrake = true;
        run(&mut app, 3.0);

        let stopped = speed(&app, car);
        assert!(stopped.length() < 0.2, "still doing {:.2} m/s", stopped.length());
        assert!(stopped.z < 0.2, "the brakes pushed it backwards at {:.2} m/s", -stopped.z);
    }

    /// Steering right turns it right. The whole model rests on this: nothing applies a turning
    /// torque anywhere — the front tyres simply grip in a different direction, and that is what
    /// swings the back end round.
    #[test]
    fn steering_turns_it_the_way_the_wheels_point() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 0.5);
        assert!(facing(&app, car).x.abs() < 1e-3, "did not start facing straight ahead");

        hands(&mut app, car, 1.0, 1.0);
        run(&mut app, 2.0);

        let ahead = facing(&app, car);
        assert!(ahead.x > 0.2, "full right lock turned it to {:?}", ahead);
        assert!(pose(&app, car).x > 0.5, "it turned on the spot rather than driving round");
    }

    /// The drive force tapers off, or a constant push accelerates for ever.
    #[test]
    fn it_stops_accelerating_near_its_top_speed() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 0.5);
        hands(&mut app, car, 1.0, 0.0);
        run(&mut app, 12.0);

        let fast = speed(&app, car).length();
        assert!(fast > spec.top_speed * 0.7, "only reached {fast:.1} m/s");
        assert!(fast < spec.top_speed * 1.1, "ran away to {fast:.1} m/s");
    }

    /// A tyre can only hold so much sideways force, and that limit is what makes a fast corner
    /// understeer rather than scrub the vehicle to a halt. Before the load cap existed, full lock
    /// at 13 m/s stopped it dead in three seconds — measured on a live server.
    #[test]
    fn a_hard_corner_understeers_rather_than_stopping_it() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());
        run(&mut app, 0.5);
        hands(&mut app, car, 1.0, 0.0);
        run(&mut app, 4.0);
        let straight = speed(&app, car).length();
        assert!(straight > 10.0, "never got up to speed: {straight:.1} m/s");

        hands(&mut app, car, 1.0, 1.0);
        run(&mut app, 2.0);

        let cornering = speed(&app, car).length();
        assert!(
            cornering > straight * 0.5,
            "the corner scrubbed it from {straight:.1} to {cornering:.1} m/s"
        );
        assert!(facing(&app, car).x > 0.2, "and it did not even turn");
    }

    /// The mass has to end up below the middle of the body, or the vehicle tips over in the first
    /// corner. Avian combines several sources into the total, so this asks the engine what it
    /// actually computed rather than trusting that setting the component was enough.
    #[test]
    fn the_mass_sits_low_in_the_body() {
        let mut app = driving_app();
        let spec = VehicleKind::Buggy.spec();
        let car = park(&mut app, Vec3::Y * spec.ride_height());

        let centre = app
            .world()
            .get::<ComputedCenterOfMass>(car)
            .expect("a computed centre of mass")
            .0;
        assert!(
            centre.y < -0.1,
            "the mass sits at y = {:.3} in the body: nothing pulled it down",
            centre.y
        );
    }
}
