//! The physics world, and the level queries the movement code makes of it.
//!
//! Avian is here as a *query* engine first. Level geometry is static, players are not rigid bodies,
//! and nothing in this module runs a solver step for gameplay — [`Level`] casts rays and sweeps a
//! capsule, and that is all. `PlayerState` stays four fields that a rollback can restore, which is
//! the property the whole prediction scheme rests on.
//!
//! The solver is nonetheless installed, because everything that is *not* a player wants it: crates
//! that can be pushed, doors, vehicles, ragdolls. With no dynamic bodies in the world it does no
//! work. See the README, "Two kinds of physics", for why the line is drawn between the player and
//! everything else rather than between two libraries.
//!
//! ### Traps
//!
//! Avian answers wrongly rather than loudly when it is set up wrongly. Two cost an afternoon:
//!
//! - **`MoveAndSlide` only sees colliders attached to a rigid body.** Its collider query is filtered
//!   `With<ColliderOf>` and used as the predicate for every cast it makes, so a bare [`Collider`] is
//!   invisible to sweeps while staying visible to `SpatialQuery::cast_ray`. Level geometry carries
//!   [`RigidBody::Static`] for that reason and no other.
//! - **A collider is placed by [`Position`], not `Transform`.** `Transform` reaches `Position`
//!   through a system, so a collider spawned with only a transform sits at the origin until that
//!   system has run — which, for something spawned in `Startup` and queried in `FixedUpdate`, is
//!   after the first tick of movement.
//!
//! `Collider::cuboid` also takes full side lengths where rapier's takes half-extents.

use avian3d::prelude::*;
use bevy::ecs::system::SystemParam;
use bevy::prelude::*;

use crate::movement::{
    CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET, CROUCH_CAPSULE_HALF_HEIGHT,
    CROUCH_CAPSULE_Y_OFFSET, GROUND_SNAP_DIST, MAX_SLOPE_LIFT, SKIN, WALKABLE_NORMAL_Y, slope_lift,
};

/// Downward acceleration on a dynamic body, in metres per second squared.
///
/// Not [`movement::GRAVITY`](crate::movement::GRAVITY), which is a game-feel number tuned for how a
/// player's jump should arc and is deliberately far heavier than the world's. A crate falls at the
/// rate a crate falls.
pub const WORLD_GRAVITY: f32 = -9.81;

/// What a collider is, for the purpose of deciding which queries should see it.
///
/// [`Level`] asks only about [`Layer::Level`]. Without that filter every ground probe and every
/// bullet would start hitting players and vehicles the moment those gain colliders, and the failure
/// would be a subtle one: the ground under your feet is another player.
///
/// [`Layer::Body`] is the default so that the *loud* mistake is the likely one. Forget to label
/// level geometry and you fall through the world on the first tick; forget to label a player and it
/// is merely not walkable.
#[derive(PhysicsLayer, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// Players, props, vehicles — anything that is not the map.
    #[default]
    Body,
    /// The map: ground, walls, and the crates that never move.
    Level,
}

/// Installs Avian.
///
/// `FixedPostUpdate` puts the physics step inside `FixedMain`, which is what lightyear re-runs once
/// per replayed tick during a rollback. Anything scheduled outside it would be replayed zero times
/// or once, never in step.
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugins::new(FixedPostUpdate));
    }
}

/// Static level geometry, ready to spawn.
///
/// A bundle rather than a free function so that both sides build the same entity from the same
/// place: client and server must collide against identical shapes, or the client's prediction and
/// the server's authority disagree about where a player can stand and every step near the
/// difference is a stutter.
pub fn level_geometry(collider: Collider, at: Vec3) -> impl Bundle {
    level_geometry_facing(collider, at, Quat::IDENTITY)
}

/// The same, for geometry that is not axis-aligned: a ramp, a sloped roof, a leaning wall.
///
/// The rotation is [`Rotation`], not a `Transform`, for the same reason the position is
/// [`Position`] — those are what Avian places a collider by, and a shape spawned with only a
/// transform sits unrotated at the origin until a sync system has run.
pub fn level_geometry_facing(collider: Collider, at: Vec3, facing: Quat) -> impl Bundle {
    (
        RigidBody::Static,
        collider,
        Position(at),
        Rotation(facing),
        CollisionLayers::new(Layer::Level, LayerMask::ALL),
    )
}

/// The player's collision capsule for a stance, and how far its centre sits above the feet.
///
/// The one place the player's shape is written down. It is what the movement sweeps with and what a
/// shot is tested against — [`Hitbox::of`](crate::hitbox::Hitbox::of) builds from this — so the two
/// cannot disagree about how big a player is.
pub fn player_capsule(crouching: bool) -> (Collider, f32) {
    let (half_height, y_offset) = if crouching {
        (CROUCH_CAPSULE_HALF_HEIGHT, CROUCH_CAPSULE_Y_OFFSET)
    } else {
        (CAPSULE_HALF_HEIGHT, CAPSULE_Y_OFFSET)
    };
    // Avian's capsule length is the cylinder between the caps, so it is twice the half height.
    (Collider::capsule(CAPSULE_RADIUS, half_height * 2.0), y_offset)
}

/// Queries against the level, and nothing else.
///
/// The replacement for the old `CollisionWorld` resource: same questions, same answers (a test
/// holds the two to account while both exist), but backed by the ECS, so a collider can move.
///
/// Every method here is a pure function of its arguments and the current collider trees. That is
/// what makes it safe to call while replaying a rollback: the same tick replayed twice asks the
/// same question and gets the same answer.
#[derive(SystemParam)]
pub struct Level<'w, 's> {
    slide: MoveAndSlide<'w, 's>,
}

impl Level<'_, '_> {
    /// What a foot can stand on: the map, and the things in it.
    ///
    /// Wider than [`sight_line`](Self::sight_line), and the difference is the point. A crate is
    /// something to climb; it is not something a bullet stops at *here*, because a shot tests every
    /// hitbox separately and takes the nearest — counting the crate twice would let the wall test
    /// win over the target test at the same distance and turn a hit into a miss.
    fn footing() -> SpatialQueryFilter {
        SpatialQueryFilter::from_mask(LayerMask::from([Layer::Level, Layer::Body]))
    }

    /// What stops a bullet on its way to a target: the map alone. See [`footing`](Self::footing).
    fn sight_line() -> SpatialQueryFilter {
        SpatialQueryFilter::from_mask(Layer::Level)
    }

    /// The sweep keeps the same skin as the rest of the movement code, rather than Avian's own
    /// default, so that "one skin clear of the surface" means one thing in both places.
    fn sweep_config() -> MoveAndSlideConfig {
        MoveAndSlideConfig { skin_width: SKIN, ..default() }
    }

    /// Moves the player capsule by `delta`, sliding along whatever it hits, and returns the
    /// displacement actually achieved.
    ///
    /// `feet` is the position of the player's feet, not the capsule centre.
    pub fn sweep_capsule(&self, feet: Vec3, delta: Vec3, crouching: bool) -> Vec3 {
        let (shape, y_offset) = player_capsule(crouching);
        let start = feet + Vec3::Y * y_offset;
        // `move_and_slide` is given a velocity and a duration; handing it the whole displacement
        // over one second asks for exactly that displacement.
        self.slide
            .move_and_slide(
                &shape,
                start,
                Quat::IDENTITY,
                delta,
                core::time::Duration::from_secs(1),
                &Self::sweep_config(),
                &Self::footing(),
                |_| MoveAndSlideHitResponse::Accept,
            )
            .position
            - start
    }

    /// True when the player is standing on something they can stand on.
    ///
    /// See [`footing_below`](Self::footing_below), which answers the same question and also says
    /// where the feet belong.
    pub fn is_grounded(&self, feet: Vec3, _crouching: bool) -> bool {
        self.footing_below(feet).is_some()
    }

    /// Where the feet rest, if there is ground under them that is theirs to stand on.
    ///
    /// `None` on three counts, and the movement code treats all three the same way — as falling:
    /// nothing below, ground out of reach, or ground too steep to hold a player
    /// ([`WALKABLE_NORMAL_Y`]).
    ///
    /// The height that comes back is not the surface: it is [`slope_lift`] above it, because a
    /// round capsule on a slope touches the ground beside the point its feet are over, not under
    /// it. Putting the feet on the surface itself would drive the capsule into the hill for the
    /// next sweep to push back out — visible as a jitter, and the reason this returns a resting
    /// height rather than a ground height.
    ///
    /// **And one skin clear of the surface, measured the way the sweep measures it.** The sweep
    /// keeps `SKIN` *perpendicular* to whatever it touches; this returns a height along *y*. On
    /// flat ground the two are the same distance and it does not matter. On a slope they are not,
    /// and a skin added straight up leaves the capsule nearer the hill than the sweep wants it —
    /// so every tick the sweep pushed it out along the normal and the snap pulled it back down,
    /// and the pair of them walked a standing player downhill at 13 cm a second on a 45° bank.
    /// Dividing by `normal.y` is what turns a perpendicular clearance into a vertical one.
    ///
    /// The reach is still measured to the resting height itself: how far the probe looks is a
    /// question about the ground, and the clearance is a question about the capsule.
    pub fn footing_below(&self, feet: Vec3) -> Option<f32> {
        let (ground, normal) = self.ground_below(feet)?;
        if normal.y < WALKABLE_NORMAL_Y {
            return None;
        }
        let rest = ground + slope_lift(normal.y);
        (feet.y - rest <= GROUND_SNAP_DIST).then(|| rest + SKIN / normal.y)
    }

    /// Height of the ground directly beneath `feet`, searched from a little above and a little
    /// below, with the direction that surface faces.
    ///
    /// This is a ray, not a shape cast, and deliberately so — twice over. Shape casts against a
    /// triangle mesh are accurate to a few millimetres at best, and using one here produced errors
    /// of up to 11 cm, worse than the drift it was meant to correct. A shape cast also reported
    /// "airborne" on roughly one tick in seven while walking on flat ground, which made the player
    /// oscillate between walking and air speed. A ray reports an exact intersection.
    ///
    /// The trade-off is the ray's blindness to the capsule's width: standing with the centre past a
    /// ledge counts as airborne even though the capsule still rests on the edge. Sampling a few rays
    /// around the capsule would fix that when it matters.
    ///
    /// It reaches [`MAX_SLOPE_LIFT`] further down than the snap distance, because on a slope the
    /// feet float that far clear of the surface; how much of that reach counts as footing is
    /// [`footing_below`](Self::footing_below)'s business, not this one's.
    pub fn ground_below(&self, feet: Vec3) -> Option<(f32, Vec3)> {
        let origin = feet + Vec3::Y * GROUND_SNAP_DIST;
        let direction = Dir3::NEG_Y;
        let hit = self.slide.spatial_query.cast_ray(
            origin,
            direction,
            GROUND_SNAP_DIST * 2.0 + MAX_SLOPE_LIFT,
            true,
            &Self::footing(),
        )?;
        Some((origin.y - hit.distance, hit.normal))
    }

    /// Height of the ground directly beneath `feet`, whatever it faces.
    pub fn ground_height_below(&self, feet: Vec3) -> Option<f32> {
        self.ground_below(feet).map(|(ground, _)| ground)
    }

    /// Distance to the first piece of level geometry along a ray, if any within `max_distance`.
    ///
    /// What a shot stops at. Players are not on the level layer, so a hitscan tests this for the
    /// wall behind the target and the target's own hitbox separately, and takes whichever is nearer.
    pub fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<f32> {
        self.raycast_normal(origin, direction, max_distance)
            .map(|(distance, _)| distance)
    }

    /// Where a ray meets the level, and which way that surface faces.
    ///
    /// The normal is what a bullet hole needs: a decal has to lie *on* the wall, and a mark that
    /// merely faced the shooter would stand off the floor edge-on at a grazing angle. Every client
    /// builds this world from the same numbers as the server, so a client can ask this for itself
    /// rather than having the answer sent to it.
    pub fn raycast_normal(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<(f32, Vec3)> {
        let direction = Dir3::new(direction).ok()?;
        let hit = self.slide.spatial_query.cast_ray(
            origin,
            direction,
            max_distance,
            true,
            &Self::sight_line(),
        )?;
        Some((hit.distance, hit.normal))
    }

    /// Where a shot leaves a mark, and on what.
    ///
    /// Wider than [`raycast`](Self::raycast) on purpose, and the difference is the whole reason
    /// both exist. What *stops* a bullet is the map: a crate is tested as a hitbox instead, so
    /// counting it twice would let the wall test beat the target test. What a bullet leaves a
    /// *mark* on is anything solid it can end against — a wall, a crate, a vehicle.
    ///
    /// The entity comes back with it so a decal can be hung on what it hit rather than left
    /// floating in world space where that thing used to be.
    pub fn surface_hit(
        &self,
        origin: Vec3,
        direction: Vec3,
        max_distance: f32,
    ) -> Option<(f32, Vec3, Entity)> {
        let direction = Dir3::new(direction).ok()?;
        let hit = self.slide.spatial_query.cast_ray(
            origin,
            direction,
            max_distance,
            true,
            &Self::footing(),
        )?;
        Some((hit.distance, hit.normal, hit.entity))
    }

    /// True when there is room to stand up from a crouch.
    ///
    /// Only the section a standing capsule adds on top needs to be clear, so this sweeps a ball of
    /// the capsule radius upward from the crouched centre — that ball is exactly the volume the
    /// capsule's cap passes through.
    pub fn can_stand_up(&self, feet: Vec3) -> bool {
        let rise = CAPSULE_Y_OFFSET + CAPSULE_HALF_HEIGHT
            - (CROUCH_CAPSULE_Y_OFFSET + CROUCH_CAPSULE_HALF_HEIGHT);
        let top_of_crouched =
            feet + Vec3::Y * (CROUCH_CAPSULE_Y_OFFSET + CROUCH_CAPSULE_HALF_HEIGHT);

        self.slide
            .spatial_query
            .cast_shape(
                &Collider::sphere(CAPSULE_RADIUS),
                top_of_crouched,
                Quat::IDENTITY,
                Dir3::Y,
                &ShapeCastConfig::from_max_distance(rise),
                &Self::footing(),
            )
            .is_none()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// An app with Avian and a floor at y = 0, for anything that has to collide with something.
    ///
    /// The floor is big enough that a test can walk for a minute without reaching the edge — at
    /// 5.5 m/s that is 330 m, and falling off the world looks exactly like a physics bug.
    pub fn floor_app() -> App {
        let mut app = unfinished();
        app.world_mut().spawn(level_geometry(
            Collider::trimesh(
                vec![
                    Vec3::new(-1000.0, 0.0, -1000.0),
                    Vec3::new(1000.0, 0.0, -1000.0),
                    Vec3::new(1000.0, 0.0, 1000.0),
                    Vec3::new(-1000.0, 0.0, 1000.0),
                ],
                vec![[0, 1, 2], [0, 2, 3]],
            ),
            Vec3::ZERO,
        ));
        ready(app)
    }

    /// An app whose whole world is one slope of `angle` radians, climbing toward −Z, passing
    /// through the origin.
    ///
    /// A tilted slab rather than a height field on purpose: its normal is known exactly, so a test
    /// that asks what the movement code does at 30° is asking about 30° and not about the
    /// discretisation of a grid. It is what the ramp already is, only steeper.
    pub fn slope_app(angle: f32) -> App {
        let mut app = unfinished();
        let (sin, cos) = angle.sin_cos();
        // Turning about +X takes the slab's up to (0, cos, sin), and pushing the slab half its
        // thickness down that direction puts its top surface through the origin.
        let up = Vec3::new(0.0, cos, sin);
        app.world_mut().spawn(level_geometry_facing(
            Collider::cuboid(400.0, 10.0, 400.0),
            -up * 5.0,
            Quat::from_rotation_x(angle),
        ));
        ready(app)
    }

    /// An app with Avian and an empty world, for anything that needs a [`Level`] but nothing in it.
    pub fn bare_app() -> App {
        ready(unfinished())
    }

    fn unfinished() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, PhysicsPlugin));
        app
    }

    /// Finishes building an app and lets its observers run, so its colliders are queryable.
    ///
    /// Safe to call again after spawning more geometry, which is how a test adds a wall to a floor.
    ///
    /// `App::finish` is where Avian registers resources its own systems then expect. Only
    /// `App::run` calls it, so a test that drives schedules itself has to say so — and when it does
    /// not, the failure is silent: colliders are never sized, and every shape behaves as a point at
    /// its own centre.
    pub fn ready(mut app: App) -> App {
        app.finish();
        app.cleanup();
        app.update();
        app
    }

    /// Runs `query` against the app's level.
    pub fn ask<R: Send + 'static>(
        app: &mut App,
        query: impl FnMut(&Level) -> R + Send + Sync + 'static,
    ) -> R {
        use bevy::ecs::system::RunSystemOnce;
        let mut query = query;
        app.world_mut()
            .run_system_once(move |level: Level| query(&level))
            .expect("level query")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn a_ray_into_the_floor_comes_back_with_the_floor_pointing_up() {
        let mut app = floor_app();
        let (distance, normal) = ask(&mut app, |level| {
            level.raycast_normal(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 10.0)
        })
        .expect("hit");
        assert!((distance - 5.0).abs() < 1e-3, "{distance}");
        assert!(normal.dot(Vec3::Y).abs() > 0.99, "{normal:?}");
    }

    #[test]
    fn unobstructed_motion_is_unchanged() {
        let mut app = floor_app();
        let moved = ask(&mut app, |level| {
            level.sweep_capsule(Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 0.0, 0.0), false)
        });
        assert!((moved - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "{moved:?}");
    }

    #[test]
    fn falling_stops_at_the_floor() {
        let mut app = floor_app();
        let moved = ask(&mut app, |level| {
            level.sweep_capsule(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -10.0, 0.0), false)
        });
        assert!(moved.y > -5.0, "fell through the floor: {moved:?}");
        assert!(moved.y < -5.0 + 0.05, "stopped too early: {moved:?}");
    }

    /// A crate on the body layer is something to stand on and *not* something the bullet ray stops
    /// at. Both halves matter: the first is what makes a prop climbable, and the second is what
    /// keeps a shot from being counted as hitting the wall in front of the target it just hit.
    #[test]
    fn a_body_can_be_stood_on_but_does_not_stop_a_bullet() {
        let mut app = floor_app();
        // A metre cube whose top is at 1 m, of the kind a loose crate is.
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(1.0, 1.0, 1.0),
            Position(Vec3::new(5.0, 0.5, 0.0)),
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
        ));
        let mut app = ready(app);

        let ground = ask(&mut app, |level| level.ground_height_below(Vec3::new(5.0, 1.05, 0.0)));
        assert!(
            ground.is_some_and(|y| (y - 1.0).abs() < 1e-3),
            "no footing on top of the crate: {ground:?}"
        );

        let shot = ask(&mut app, |level| {
            level.raycast(Vec3::new(0.0, 0.5, 0.0), Vec3::X, 20.0)
        });
        assert!(shot.is_none(), "the bullet ray stopped at a crate, at {shot:?} m");
    }

    /// And it has to stop a walk, or a crate is scenery you pass through.
    #[test]
    fn a_body_is_walked_into_rather_than_through() {
        let mut app = floor_app();
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(1.0, 1.0, 1.0),
            Position(Vec3::new(5.0, 0.5, 0.0)),
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
        ));
        let mut app = ready(app);

        let moved = ask(&mut app, |level| {
            level.sweep_capsule(Vec3::ZERO, Vec3::X * 10.0, false)
        });
        assert!(moved.x < 5.0, "walked {:.2} m, straight through the crate", moved.x);
        assert!(moved.x > 3.0, "stopped {:.2} m short of a crate five metres away", moved.x);
    }

    /// A capsule resting on the floor must still walk. Getting this wrong froze the player
    /// mid-stride while velocity read a healthy 5.5 m/s.
    #[test]
    fn a_capsule_resting_on_the_floor_can_walk() {
        let mut app = floor_app();
        let step = Vec3::new(0.0, -0.3125 / 64.0, -5.5 / 64.0);
        for height in [SKIN * 2.0, SKIN, SKIN * 0.5, 0.0, -0.0001] {
            let moved = ask(&mut app, move |level| {
                level.sweep_capsule(Vec3::new(0.0, height, 0.0), step, false)
            });
            assert!(
                moved.z < step.z * 0.9,
                "at height {height} the sweep only moved {} of a wanted {}",
                moved.z,
                step.z
            );
        }
    }

    #[test]
    fn walking_into_a_wall_slides_along_it() {
        let mut app = floor_app();
        // A wall in the x = 2 plane.
        app.world_mut().spawn(level_geometry(
            Collider::cuboid(0.2, 20.0, 100.0),
            Vec3::new(2.1, 5.0, 0.0),
        ));
        let mut app = ready(app);

        let moved = ask(&mut app, |level| {
            level.sweep_capsule(Vec3::new(0.0, 0.001, 0.0), Vec3::new(3.0, 0.0, 1.0), false)
        });
        // Blocked on X well short of the 3 m asked for...
        assert!(moved.x < 2.0, "went through the wall: {moved:?}");
        // ...but the Z component survives, which is what sliding means.
        assert!(moved.z > 0.9, "slid nothing along the wall: {moved:?}");
    }

    /// The slope limit as the ground probe sees it: a slope inside it is footing, one past it is
    /// not, and the two are one degree apart so the test is about the limit and not about slopes in
    /// general.
    #[test]
    fn a_slope_is_footing_only_while_it_is_within_the_limit() {
        let limit = WALKABLE_NORMAL_Y.acos();
        let mut walkable = slope_app(limit - 0.01);
        assert!(ask(&mut walkable, |level| level.is_grounded(Vec3::new(0.0, 0.01, 0.0), false)));
        let mut cliff = slope_app(limit + 0.01);
        assert!(!ask(&mut cliff, |level| level.is_grounded(Vec3::new(0.0, 0.01, 0.0), false)));
    }

    /// A round capsule on a slope touches the ground beside the point its feet are over, so the
    /// feet rest above the surface — and the probe has to say so, or every tick snaps the capsule
    /// into the hill for the sweep to push back out.
    ///
    /// Checked against the arithmetic rather than a measured constant, because the arithmetic is
    /// what `slope_lift` claims to be.
    ///
    /// **And the skin on top of it is `SKIN / normal.y`, not `SKIN`.** That is the second half of
    /// the same idea and it is the one that was got wrong: the sweep keeps its skin perpendicular
    /// to the surface, so a clearance added straight up is short by the slope's own cosine, and
    /// short by exactly the amount the sweep then spends every tick pushing back out. What that
    /// cost is in `standing_on_a_hillside_is_not_sliding_down_it`.
    #[test]
    fn the_feet_rest_above_a_slope_by_the_capsules_own_roundness() {
        let angle: f32 = 0.6;
        let mut app = slope_app(angle);
        let stand = ask(&mut app, |level| level.footing_below(Vec3::new(0.0, 0.01, 0.0)))
            .expect("no footing on a 34° slope");
        let want = slope_lift(angle.cos()) + SKIN / angle.cos();
        assert!((stand - want).abs() < 1e-3, "the feet stand at {stand:.4}, not {want:.4}");
    }

    /// And on the flat the lift is zero and the skin is the skin, which is what keeps this from
    /// changing anything that already worked.
    #[test]
    fn on_the_flat_the_feet_rest_one_skin_over_the_ground() {
        let mut app = floor_app();
        let stand = ask(&mut app, |level| level.footing_below(Vec3::new(0.0, 0.01, 0.0)));
        assert!(
            stand.is_some_and(|y| (y - SKIN).abs() < 1e-4),
            "the feet stand at {stand:?}, not one skin over 0",
        );
    }

    #[test]
    fn standing_on_the_floor_reads_as_grounded() {
        let mut app = floor_app();
        assert!(ask(&mut app, |level| level.is_grounded(Vec3::ZERO, false)));
        assert!(!ask(&mut app, |level| level.is_grounded(Vec3::new(0.0, 5.0, 0.0), false)));
    }

    #[test]
    fn open_space_allows_standing_up() {
        let mut app = floor_app();
        assert!(ask(&mut app, |level| level.can_stand_up(Vec3::ZERO)));
    }

    #[test]
    fn a_low_ceiling_prevents_standing_up() {
        let mut app = floor_app();
        // Ceiling at 1.3 m: clears the 1.2 m crouched capsule, blocks the 1.7 m standing one.
        app.world_mut().spawn(level_geometry(
            Collider::cuboid(10.0, 0.2, 10.0),
            Vec3::new(0.0, 1.4, 0.0),
        ));
        let mut app = ready(app);
        assert!(!ask(&mut app, |level| level.can_stand_up(Vec3::ZERO)));
    }

    /// A collider that says nothing about its layers is a body, not the level.
    ///
    /// That is the whole reason [`Layer::Body`] is the default variant, and it is worth a test of
    /// its own because it rests on the *order* of the enum: Avian's default membership is the first
    /// layer. Reorder the variants and unlabelled geometry silently becomes the map, which is the
    /// mistake this arrangement exists to prevent.
    #[test]
    fn a_collider_with_no_layer_stated_is_a_body() {
        let mut app = floor_app();
        app.world_mut().spawn((
            RigidBody::Static,
            Collider::cuboid(4.0, 4.0, 4.0),
            Position(Vec3::new(0.0, 20.0, 0.0)),
        ));
        let mut app = ready(app);
        let hit = ask(&mut app, |level| {
            level.raycast(Vec3::new(0.0, 30.0, 0.0), Vec3::NEG_Y, 100.0)
        });
        assert_eq!(hit, Some(30.0), "a ray stopped on something that is not the level");
    }

    /// The sweep must be a pure function — a rollback replays it and has to get the same answer.
    #[test]
    fn repeated_sweeps_agree() {
        let mut app = floor_app();
        let feet = Vec3::new(0.0, 0.001, 0.0);
        let delta = Vec3::new(3.0, -1.0, 1.0);
        let results = ask(&mut app, move |level| {
            (0..8).map(|_| level.sweep_capsule(feet, delta, false)).collect::<Vec<_>>()
        });
        assert!(results.windows(2).all(|w| w[0] == w[1]), "{results:?}");
    }
}

