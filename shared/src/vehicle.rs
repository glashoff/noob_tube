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
//! That is why [`suspend_vehicles`] takes a query filter, exactly as
//! [`step_players`](crate::simulation::step_players) does: the server steps every vehicle, and a
//! client will step only the one it is driving.

use avian3d::prelude::*;
use bevy::ecs::query::QueryFilter;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::physics::{Layer, WORLD_GRAVITY};

/// How many struts a vehicle has. Four, and the code says so once rather than in six places.
pub const WHEELS: usize = 4;

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
    /// Half the size of the chassis box.
    pub half_extents: Vec3,
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
}

impl VehicleSpec {
    /// Kilograms per cubic metre, so that [`ColliderDensity`] and [`mass`](Self::mass) agree by
    /// construction. Avian derives both mass and inertia from the collider and its density, so
    /// setting the mass directly would leave the inertia describing a different vehicle.
    pub fn density(&self) -> f32 {
        self.mass / (8.0 * self.half_extents.x * self.half_extents.y * self.half_extents.z)
    }

    /// The collider the chassis is, sized in full side lengths as Avian wants them.
    pub fn collider(&self) -> Collider {
        Collider::cuboid(
            self.half_extents.x * 2.0,
            self.half_extents.y * 2.0,
            self.half_extents.z * 2.0,
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
};

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
        RigidBody::Dynamic,
        spec.collider(),
        ColliderDensity(spec.density()),
        CenterOfMass(Vec3::NEG_Y * spec.centre_of_mass_drop),
        // Not on the level layer: a vehicle is not terrain, and the movement queries deliberately
        // do not see it. Walking on one is its own piece of work.
        CollisionLayers::new(Layer::Body, LayerMask::ALL),
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

/// FixedUpdate: holds every matching vehicle up on its springs, and keeps its tyres from sliding.
///
/// Runs before the solver step in `FixedPostUpdate`, which is what consumes the forces; both are
/// inside `FixedMain`, so a rollback replays this once per replayed tick exactly as it replays
/// movement. It reads nothing but its arguments and the collider trees, which is what makes that
/// replay reproduce the same result.
pub fn suspend_vehicles<F: QueryFilter + 'static>(
    space: SpatialQuery,
    time: Res<Time<Fixed>>,
    mut vehicles: Query<(Entity, &VehicleKind, &mut Wheels, Forces), F>,
) {
    let dt = time.delta_secs();

    for (entity, kind, mut wheels, mut body) in vehicles.iter_mut() {
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

        for wheel in &found {
            if !wheel.grounded {
                continue;
            }
            let velocity = body.velocity_at_point(wheel.contact);
            // Spring minus damper, and never negative: a strut can push the chassis away from the
            // ground, but it cannot pull it back down. Letting it go negative is how a car ends up
            // sucked onto the road and unable to leave a ramp.
            let load = (spec.stiffness * wheel.compression - spec.damping * velocity.dot(up)).max(0.0);
            body.apply_force_at_point(up * load, wheel.contact);

            // Tyre friction, at the contact patch rather than at the centre of mass. That is what
            // makes the body lean into a corner and dip under braking — the same force through a
            // longer lever arm — and it is also what lets it roll over if the mass sits too high.
            //
            // Both directions are flattened onto the ground first, or on a slope some of the grip
            // would act as lift.
            let forward = (rotation * Vec3::NEG_Z).reject_from(wheel.normal).normalize_or_zero();
            let right = (rotation * Vec3::X).reject_from(wheel.normal).normalize_or_zero();
            body.apply_linear_impulse_at_point(
                right * (-velocity.dot(right) * grip * share),
                wheel.contact,
            );
            body.apply_linear_impulse_at_point(
                forward * (-velocity.dot(forward) * drag * share),
                wheel.contact,
            );
        }

        wheels.0 = found;
    }
}

#[cfg(test)]
mod tests {
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
        app.add_systems(FixedUpdate, suspend_vehicles::<()>);
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
