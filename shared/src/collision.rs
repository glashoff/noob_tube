//! Stateless collision queries against static level geometry.
//!
//! This is rapier used as a query library only: colliders and a BVH, never a solver and never
//! `PhysicsPipeline::step()`. Nothing here carries state between ticks, so a rollback has nothing
//! to restore beyond the player's own fields. See the README, "Two kinds of physics".
//!
//! The sweep is a port of `sweepShape` in `webgame/shared/game/physics.ts`.

use bevy::ecs::resource::Resource;
use bevy::math::Vec3;
use rapier3d::parry::query::{DefaultQueryDispatcher, ShapeCastOptions};
use rapier3d::prelude::*;

use crate::movement::{
    CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET, CROUCH_CAPSULE_HALF_HEIGHT,
    CROUCH_CAPSULE_Y_OFFSET, GROUND_SNAP_DIST, SKIN,
};

/// How many times the sweep may hit something and slide before giving up. Three is what `webgame`
/// uses: enough for a floor plus two walls (an inside corner), cheap enough to run per tick.
const MAX_SLIDE_ITERATIONS: usize = 3;

/// Static level geometry, queryable but never simulated.
#[derive(Resource)]
pub struct CollisionWorld {
    bodies: RigidBodySet,
    colliders: ColliderSet,
    broad_phase: BroadPhaseBvh,
    dispatcher: DefaultQueryDispatcher,
    /// Colliders added since the last `rebuild`.
    pending: Vec<ColliderHandle>,
}

impl Default for CollisionWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl CollisionWorld {
    pub fn new() -> Self {
        Self {
            bodies: RigidBodySet::new(),
            colliders: ColliderSet::new(),
            broad_phase: BroadPhaseBvh::new(),
            dispatcher: DefaultQueryDispatcher,
            pending: Vec::new(),
        }
    }

    /// Adds a triangle mesh. Call [`rebuild`](Self::rebuild) once after adding everything.
    ///
    /// This takes a *collision* mesh, which should be a simplified stand-in for the render mesh —
    /// fewer triangles to sweep against, and nothing decorative to snag on.
    pub fn add_trimesh(&mut self, vertices: Vec<Vec3>, indices: Vec<[u32; 3]>) {
        let collider = ColliderBuilder::trimesh(vertices, indices)
            .expect("invalid collision mesh")
            .build();
        self.pending.push(self.colliders.insert(collider));
    }

    /// Adds a box collider, centred on `centre`, with the given half-extents.
    pub fn add_cuboid(&mut self, centre: Vec3, half_extents: Vec3) {
        let collider = ColliderBuilder::cuboid(half_extents.x, half_extents.y, half_extents.z)
            .position(Pose::from_translation(centre))
            .build();
        self.pending.push(self.colliders.insert(collider));
    }

    /// Builds the BVH over everything added so far.
    ///
    /// Level geometry is static, so this runs once at load and never again — which is also why the
    /// BVH never needs snapshotting for rollback.
    pub fn rebuild(&mut self) {
        let mut events = Vec::new();
        self.broad_phase.update(
            &IntegrationParameters::default(),
            &self.colliders,
            &self.bodies,
            &self.pending,
            &[],
            &mut events,
        );
        self.pending.clear();
    }

    fn query(&self) -> QueryPipeline<'_> {
        self.broad_phase.as_query_pipeline(
            &self.dispatcher,
            &self.bodies,
            &self.colliders,
            QueryFilter::default(),
        )
    }

    /// Capsule dimensions for a stance: (half height, centre offset above the feet).
    fn capsule_dims(crouching: bool) -> (f32, f32) {
        if crouching {
            (CROUCH_CAPSULE_HALF_HEIGHT, CROUCH_CAPSULE_Y_OFFSET)
        } else {
            (CAPSULE_HALF_HEIGHT, CAPSULE_Y_OFFSET)
        }
    }

    /// Moves the player capsule by `delta`, sliding along whatever it hits, and returns the
    /// displacement actually achieved.
    ///
    /// `feet` is the position of the player's feet, not the capsule centre.
    ///
    /// Pure function of its arguments and the static geometry: same inputs, same output, every
    /// time. That is what makes it safe to replay during a rollback.
    pub fn sweep_capsule(&self, feet: Vec3, delta: Vec3, crouching: bool) -> Vec3 {
        let (half, y_offset) = Self::capsule_dims(crouching);
        let shape = Capsule::new_y(half, CAPSULE_RADIUS);
        let query = self.query();

        let mut centre = feet + Vec3::Y * y_offset;
        let mut remaining = delta;
        let mut moved = Vec3::ZERO;
        // After a zero-distance touch we stop reporting the surface we are already against, so the
        // next cast can find the *next* obstacle — a wall while standing on the floor, say.
        let mut stop_at_penetration = true;

        for _ in 0..MAX_SLIDE_ITERATIONS {
            let length = remaining.length();
            if length < 1e-6 {
                break;
            }

            let options = ShapeCastOptions {
                max_time_of_impact: 1.0,
                stop_at_penetration,
                ..ShapeCastOptions::default()
            };

            let Some((_, hit)) =
                query.cast_shape(&Pose::from_translation(centre), remaining, &shape, options)
            else {
                moved += remaining;
                break;
            };

            let normal = Vec3::from(hit.normal1);

            if hit.time_of_impact < 1e-6 {
                // Already touching. Only cancel the motion if it points *into* the surface —
                // otherwise jumping off the floor would be impossible.
                let into = remaining.dot(normal);
                if into < 0.0 {
                    remaining -= into * normal;
                }
                stop_at_penetration = false;
                continue;
            }

            // Stop just short of contact so the capsule does not end up touching and sticking.
            let safe = (hit.time_of_impact - SKIN / length).max(0.0);
            moved += remaining * safe;
            centre += remaining * safe;

            // Project what is left onto the surface. This projection is the slide.
            remaining *= 1.0 - safe;
            remaining -= remaining.dot(normal) * normal;
            stop_at_penetration = false;
        }

        moved
    }

    /// True when solid ground sits within [`GROUND_SNAP_DIST`] below the feet.
    pub fn is_grounded(&self, feet: Vec3, crouching: bool) -> bool {
        let (half, y_offset) = Self::capsule_dims(crouching);
        let shape = Capsule::new_y(half, CAPSULE_RADIUS);
        let centre = feet + Vec3::Y * y_offset;

        self.query()
            .cast_shape(
                &Pose::from_translation(centre),
                Vec3::NEG_Y * GROUND_SNAP_DIST,
                &shape,
                ShapeCastOptions {
                    max_time_of_impact: 1.0,
                    stop_at_penetration: true,
                    ..ShapeCastOptions::default()
                },
            )
            .is_some()
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

        self.query()
            .cast_shape(
                &Pose::from_translation(top_of_crouched),
                Vec3::Y * rise,
                &Ball::new(CAPSULE_RADIUS),
                ShapeCastOptions {
                    max_time_of_impact: 1.0,
                    stop_at_penetration: false,
                    ..ShapeCastOptions::default()
                },
            )
            .is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::CAPSULE_Y_OFFSET;

    /// A 100x100 floor quad at y = 0.
    fn floor() -> CollisionWorld {
        let mut world = CollisionWorld::new();
        world.add_trimesh(
            vec![
                Vec3::new(-50.0, 0.0, -50.0),
                Vec3::new(50.0, 0.0, -50.0),
                Vec3::new(50.0, 0.0, 50.0),
                Vec3::new(-50.0, 0.0, 50.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        world.rebuild();
        world
    }

    /// Adds a wall in the x = 2 plane, facing -X.
    fn with_wall(mut world: CollisionWorld) -> CollisionWorld {
        world.add_trimesh(
            vec![
                Vec3::new(2.0, 0.0, -50.0),
                Vec3::new(2.0, 0.0, 50.0),
                Vec3::new(2.0, 10.0, 50.0),
                Vec3::new(2.0, 10.0, -50.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        world.rebuild();
        world
    }

    #[test]
    fn unobstructed_motion_is_unchanged() {
        let world = floor();
        let moved = world.sweep_capsule(Vec3::new(0.0, 5.0, 0.0), Vec3::new(1.0, 0.0, 0.0), false);
        assert!((moved - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-4, "{moved:?}");
    }

    #[test]
    fn falling_stops_at_the_floor() {
        let world = floor();
        let moved = world.sweep_capsule(Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -10.0, 0.0), false);
        // Falls the 5 m to the floor, less the skin, and no further.
        assert!(moved.y > -5.0, "fell through the floor: {moved:?}");
        assert!(moved.y < -5.0 + 0.05, "stopped too early: {moved:?}");
    }

    #[test]
    fn walking_into_a_wall_slides_along_it() {
        let world = with_wall(floor());
        // Start clear of the wall and push diagonally into it.
        let feet = Vec3::new(0.0, 0.001, 0.0);
        let moved = world.sweep_capsule(feet, Vec3::new(3.0, 0.0, 1.0), false);

        // Blocked on X well short of the 3 m asked for...
        assert!(moved.x < 2.0, "went through the wall: {moved:?}");
        // ...but the Z component survives, which is what sliding means.
        assert!(moved.z > 0.9, "slid nothing along the wall: {moved:?}");
    }

    #[test]
    fn standing_on_the_floor_reads_as_grounded() {
        let world = floor();
        assert!(world.is_grounded(Vec3::new(0.0, 0.0, 0.0), false));
        assert!(!world.is_grounded(Vec3::new(0.0, 5.0, 0.0), false));
    }

    #[test]
    fn open_space_allows_standing_up() {
        let world = floor();
        assert!(world.can_stand_up(Vec3::ZERO));
    }

    #[test]
    fn a_low_ceiling_prevents_standing_up() {
        let mut world = floor();
        // Ceiling at 1.3 m: clears the 1.2 m crouched capsule, blocks the 1.7 m standing one.
        world.add_trimesh(
            vec![
                Vec3::new(-5.0, 1.3, -5.0),
                Vec3::new(5.0, 1.3, -5.0),
                Vec3::new(5.0, 1.3, 5.0),
                Vec3::new(-5.0, 1.3, 5.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        world.rebuild();
        assert!(!world.can_stand_up(Vec3::ZERO));
    }

    /// The sweep must be a pure function — a rollback replays it and has to get the same answer.
    #[test]
    fn repeated_sweeps_agree() {
        let world = with_wall(floor());
        let feet = Vec3::new(0.0, 0.001, 0.0);
        let delta = Vec3::new(3.0, -1.0, 1.0);
        let first = world.sweep_capsule(feet, delta, false);
        for _ in 0..8 {
            assert_eq!(first, world.sweep_capsule(feet, delta, false));
        }
        let _ = CAPSULE_Y_OFFSET;
    }
}
