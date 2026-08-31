//! Stateless collision queries against static level geometry, in rapier.
//!
//! **Superseded by [`physics::Level`](crate::physics::Level), and used by nothing but the test that
//! compares the two.** It is kept only for that: `shared/tests/avian_matches_rapier.rs` asks both
//! engines every question the movement code makes and holds the answers to each other. Both go when
//! rapier does.
//!
//! What it was: rapier as a query library only — colliders and a BVH, never a solver and never
//! `PhysicsPipeline::step()`. Its one limitation is what ended it. The BVH is built once and cannot
//! be refit, so no collider in it can ever move, and a lift, a door or a vehicle is not expressible.
//!
//! The sweep is a port of `sweepShape` in `webgame/shared/game/physics.ts`, and the comments in it
//! are worth keeping until the Avian version has earned the same scars.

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
        // `stop_at_penetration` does NOT make the next cast skip a surface already touched — an
        // assumption carried over from the webgame port that cost an afternoon. See the contact
        // branch below for what actually keeps the loop moving.
        let mut stop_at_penetration = true;
        let mut previous_contact: Option<Vec3> = None;

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

            // A hit that would advance less than the skin counts as contact rather than travel.
            // Advancing by it would push the capsule into the surface; advancing by zero and then
            // projecting is the only safe move.
            //
            // The threshold has to be measured in distance, not in time of impact: at 5.5 m/s a
            // tick covers 0.086 m, so the skin is 12% of the step, and a fixed `toi < 1e-6` test
            // never fires. Walking along the floor then hit the surface every iteration with no
            // progress, and since a horizontal motion is perpendicular to the floor normal the
            // projection removed nothing either — the player froze in place.
            if hit.time_of_impact * length < SKIN {
                let into = remaining.dot(normal);
                if into < 0.0 {
                    remaining -= into * normal;
                }

                // The capsule settles a fraction of a millimetre into the floor, so every cast
                // from here reports that same contact at toi 0 — `stop_at_penetration` does not
                // suppress it. Without this check all three iterations report the floor, none of
                // them advances, and the player freezes mid-stride while velocity reads a healthy
                // 5.5 m/s. Seeing the same normal twice means the motion is now tangential to it,
                // and sliding along a surface we are already inside cannot go deeper.
                if previous_contact.is_some_and(|p| p.dot(normal) > 0.999) {
                    moved += remaining;
                    break;
                }
                previous_contact = Some(normal);
                stop_at_penetration = false;
                continue;
            }
            previous_contact = None;

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
    ///
    /// Uses the same ray as [`ground_height_below`](Self::ground_height_below) rather than a
    /// downward shape cast. A shape cast reported "airborne" on roughly one tick in seven while
    /// walking on flat ground, which made the player oscillate between walking and air speed.
    ///
    /// The trade-off is the ray's blindness to the capsule's width: standing with the centre past
    /// a ledge counts as airborne even though the capsule still rests on the edge. Sampling a few
    /// rays around the capsule would fix that when it matters.
    pub fn is_grounded(&self, feet: Vec3, _crouching: bool) -> bool {
        self.ground_height_below(feet)
            .is_some_and(|ground| feet.y - ground <= GROUND_SNAP_DIST)
    }

    /// Height of the ground directly beneath `feet`, searched from a little above and a little
    /// below.
    ///
    /// This is a ray, not a shape cast, and deliberately so: shape casts against a triangle mesh
    /// are accurate to a few millimetres at best, and using one here produced errors of up to
    /// 11 cm — worse than the drift it was meant to correct. A ray reports an exact intersection.
    ///
    /// The trade-off is that a single ray ignores the capsule's width, so on a ledge it reports
    /// whatever is under the centre. That is fine for holding a standing player at surface level,
    /// which is all it is used for.
    pub fn ground_height_below(&self, feet: Vec3) -> Option<f32> {
        let origin = feet + Vec3::Y * GROUND_SNAP_DIST;
        let ray = Ray::new(origin.into(), Vec3::NEG_Y.into());
        let (_, toi) = self
            .query()
            .cast_ray(&ray, GROUND_SNAP_DIST * 2.0, true)?;
        Some(origin.y - toi)
    }

    /// Distance to the first piece of level geometry along a ray, if any within `max_distance`.
    ///
    /// What a shot stops at. Players are not in this world — it holds the level and nothing else —
    /// so a hitscan tests this for the wall behind the target and the target's own capsule
    /// separately, and takes whichever is nearer.
    pub fn raycast(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<f32> {
        let ray = Ray::new(origin.into(), direction.into());
        self.query()
            .cast_ray(&ray, max_distance, true)
            .map(|(_, toi)| toi)
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
        let ray = Ray::new(origin.into(), direction.into());
        let (_, hit) = self
            .query()
            .cast_ray_and_get_normal(&ray, max_distance, true)?;
        Some((hit.time_of_impact, hit.normal.into()))
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
            // Big enough that a test can walk for a minute without reaching the edge — at
            // 5.5 m/s that is 330 m, and falling off would look exactly like a physics bug.
            vec![
                Vec3::new(-1000.0, 0.0, -1000.0),
                Vec3::new(1000.0, 0.0, -1000.0),
                Vec3::new(1000.0, 0.0, 1000.0),
                Vec3::new(-1000.0, 0.0, 1000.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        world.rebuild();
        world
    }

    /// A bullet hole has to lie on the surface it hit, so the normal has to point out of it.
    #[test]
    fn a_ray_into_the_floor_comes_back_with_the_floor_pointing_up() {
        let (distance, normal) =
            floor().raycast_normal(Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y, 10.0).expect("hit");
        assert!((distance - 5.0).abs() < 1e-3, "{distance}");
        assert!(normal.dot(Vec3::Y).abs() > 0.99, "{normal:?}");
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

    /// A capsule resting on the floor must still walk. Getting this wrong froze the player
    /// mid-stride while velocity read a healthy 5.5 m/s — the slide loop kept reporting the same
    /// floor contact and never advanced.
    ///
    /// The heights checked here are the ones that actually occur: `PlayerState::apply_input` holds
    /// a grounded capsule one skin above the surface. Deeper penetration degrades — at 5 mm inside
    /// the floor the sweep achieves about half its step — which is precisely why the player is
    /// kept clear of the surface rather than resting on it.
    #[test]
    fn a_capsule_resting_on_the_floor_can_walk() {
        let world = floor();
        let step = Vec3::new(0.0, -0.3125 / 64.0, -5.5 / 64.0);
        for height in [SKIN * 2.0, SKIN, SKIN * 0.5, 0.0, -0.0001] {
            let moved = world.sweep_capsule(Vec3::new(0.0, height, 0.0), step, false);
            assert!(
                moved.z < step.z * 0.9,
                "at height {height} the sweep only moved {} of a wanted {}",
                moved.z,
                step.z
            );
        }
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
