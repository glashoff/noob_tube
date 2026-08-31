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
    CROUCH_CAPSULE_Y_OFFSET, GROUND_SNAP_DIST, SKIN,
};

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
    (
        RigidBody::Static,
        collider,
        Position(at),
        CollisionLayers::new(Layer::Level, LayerMask::ALL),
    )
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
    /// Level geometry only. See [`Layer`].
    fn filter() -> SpatialQueryFilter {
        SpatialQueryFilter::from_mask(Layer::Level)
    }

    /// The sweep keeps the same skin as the rest of the movement code, rather than Avian's own
    /// default, so that "one skin clear of the surface" means one thing in both places.
    fn sweep_config() -> MoveAndSlideConfig {
        MoveAndSlideConfig { skin_width: SKIN, ..default() }
    }

    /// The player's collision capsule for a stance.
    fn capsule(crouching: bool) -> (Collider, f32) {
        let (half_height, y_offset) = if crouching {
            (CROUCH_CAPSULE_HALF_HEIGHT, CROUCH_CAPSULE_Y_OFFSET)
        } else {
            (CAPSULE_HALF_HEIGHT, CAPSULE_Y_OFFSET)
        };
        // Avian's capsule length is the cylinder between the caps, so it is twice the half height.
        (Collider::capsule(CAPSULE_RADIUS, half_height * 2.0), y_offset)
    }

    /// Moves the player capsule by `delta`, sliding along whatever it hits, and returns the
    /// displacement actually achieved.
    ///
    /// `feet` is the position of the player's feet, not the capsule centre.
    pub fn sweep_capsule(&self, feet: Vec3, delta: Vec3, crouching: bool) -> Vec3 {
        let (shape, y_offset) = Self::capsule(crouching);
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
                &Self::filter(),
                |_| MoveAndSlideHitResponse::Accept,
            )
            .position
            - start
    }

    /// True when solid ground sits within [`GROUND_SNAP_DIST`] below the feet.
    ///
    /// Uses the same ray as [`ground_height_below`](Self::ground_height_below) rather than a
    /// downward shape cast. A shape cast reported "airborne" on roughly one tick in seven while
    /// walking on flat ground, which made the player oscillate between walking and air speed.
    ///
    /// The trade-off is the ray's blindness to the capsule's width: standing with the centre past a
    /// ledge counts as airborne even though the capsule still rests on the edge. Sampling a few rays
    /// around the capsule would fix that when it matters.
    pub fn is_grounded(&self, feet: Vec3, _crouching: bool) -> bool {
        self.ground_height_below(feet)
            .is_some_and(|ground| feet.y - ground <= GROUND_SNAP_DIST)
    }

    /// Height of the ground directly beneath `feet`, searched from a little above and a little
    /// below.
    ///
    /// This is a ray, not a shape cast, and deliberately so: shape casts against a triangle mesh are
    /// accurate to a few millimetres at best, and using one here produced errors of up to 11 cm —
    /// worse than the drift it was meant to correct. A ray reports an exact intersection.
    pub fn ground_height_below(&self, feet: Vec3) -> Option<f32> {
        let origin = feet + Vec3::Y * GROUND_SNAP_DIST;
        let distance = self.raycast(origin, Vec3::NEG_Y, GROUND_SNAP_DIST * 2.0)?;
        Some(origin.y - distance)
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
            &Self::filter(),
        )?;
        Some((hit.distance, hit.normal))
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
                &Self::filter(),
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

    /// Level queries must not see anything that is not the level. Once players and vehicles carry
    /// colliders, a ground probe that found one would put solid floor under a player's feet
    /// wherever another player stood.
    #[test]
    fn a_body_is_not_the_ground() {
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

