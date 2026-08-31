//! What a shot can hit: a collider, and where it was.
//!
//! Everything the server tests a shot against is one of these, players and props alike. It has been
//! narrowed twice. It began as a feet position and a `crouching` flag — a player and nothing else,
//! with the shape hard-coded in the hit test. It then became an enum with one variant per kind of
//! target, which is the same problem one level up: a vehicle needs a new variant, a per-bone hitbox
//! needs another, and each brings its own arm in the ray cast.
//!
//! Now it holds an actual [`Collider`]. Anything Avian can express is a target, and the hit test is
//! one call with no cases in it. That is what makes a vehicle possible without touching this file.
//!
//! The shape travels *with* the pose rather than being looked up on the entity, because it changes
//! over time and the whole point of rewinding is to test against the shape that was there then.
//! Crouching was already an example before any prop existed: someone who ducked half a round trip
//! ago must still be standing in the past the shooter aimed at. Cloning a collider to carry it is
//! two atomic increments — the shape itself is shared, not copied.

use avian3d::prelude::Collider;
use bevy::prelude::*;

use crate::physics::player_capsule;
use crate::player::PlayerState;

/// A shape at a pose, for one tick.
#[derive(Clone, Debug)]
pub struct Hitbox {
    /// The shape. Shared rather than owned: cloning this is an `Arc` bump.
    pub collider: Collider,
    /// Where the shape's origin is, in world space.
    pub position: Vec3,
    /// Which way it faces.
    pub rotation: Quat,
}

impl Hitbox {
    pub fn new(collider: Collider, position: Vec3, rotation: Quat) -> Self {
        Self { collider, position, rotation }
    }

    /// The capsule a player presents right now.
    ///
    /// Derived from [`PlayerState`] rather than read off the entity, because that is already the
    /// authority on where a player is and how they are standing — and it is what keeps a player
    /// from needing a second, redundant copy of its own pose. See the README, "Two kinds of
    /// physics": a player is deliberately not a physics body.
    pub fn of(state: &PlayerState) -> Self {
        let (collider, y_offset) = player_capsule(state.crouching);
        Self::new(collider, state.position + Vec3::Y * y_offset, Quat::IDENTITY)
    }

    /// Where the shape is, for saying how far it has moved.
    pub fn centre(&self) -> Vec3 {
        self.position
    }

    /// Distance along the ray at which it enters this shape, if it does.
    pub fn cast_ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<f32> {
        // `solid` so a shot fired from inside a shape counts as an immediate hit rather than
        // passing through and striking the far wall of it.
        self.collider
            .cast_ray(self.position, self.rotation, origin, direction, max_distance, true)
            .map(|(distance, _)| distance)
    }

    /// Blends two moments of the same target.
    ///
    /// Used both by a client drawing a prop between two received updates and by the server
    /// rebuilding that same blend to rewind it. Only the *pose* is blended, and the shape is taken
    /// from `start`: a shape is discrete, and half a crouch is not a stance. Holding `start`'s is
    /// the choice that never shows a shape before the moment it appeared.
    ///
    /// Rotation blends the short way round, which is free capability the enum did not have: a door
    /// that swings now rewinds to the angle it was actually at.
    pub fn lerp(start: &Self, end: &Self, t: f32) -> Self {
        Self {
            collider: start.collider.clone(),
            position: start.position.lerp(end.position, t),
            rotation: start.rotation.slerp(end.rotation, t),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::{CAPSULE_RADIUS, EYE_HEIGHT};

    const FAR: f32 = 200.0;

    fn player(feet: Vec3, crouching: bool) -> Hitbox {
        Hitbox::of(&PlayerState { position: feet, crouching, ..PlayerState::default() })
    }

    fn box_at(centre: Vec3, half_extents: Vec3) -> Hitbox {
        Hitbox::new(
            Collider::cuboid(half_extents.x * 2.0, half_extents.y * 2.0, half_extents.z * 2.0),
            centre,
            Quat::IDENTITY,
        )
    }

    #[test]
    fn a_shot_down_the_middle_enters_the_capsule() {
        let hit = player(Vec3::new(0.0, 0.0, -10.0), false)
            .cast_ray(Vec3::new(0.0, EYE_HEIGHT, 0.0), Vec3::NEG_Z, FAR)
            .expect("a target straight ahead was not hit");
        assert!(hit > 9.0 && hit < 10.0, "entered at {hit}");
    }

    /// A crouched player is a smaller target, which is the point of crouching.
    #[test]
    fn crouching_ducks_under_a_level_shot() {
        let feet = Vec3::new(0.0, 0.0, -10.0);
        let eye = Vec3::new(0.0, EYE_HEIGHT, 0.0);
        assert!(player(feet, false).cast_ray(eye, Vec3::NEG_Z, FAR).is_some());
        assert!(
            player(feet, true).cast_ray(eye, Vec3::NEG_Z, FAR).is_none(),
            "a crouched player was hit by a shot at standing head height"
        );
    }

    /// Something that is not a player, and never was a variant of anything.
    #[test]
    fn a_box_is_hit_on_its_near_face() {
        let crate_ = box_at(Vec3::new(0.0, 3.0, -8.0), Vec3::splat(0.5));
        let eye = Vec3::new(0.0, 3.0, 0.0);
        let hit = crate_.cast_ray(eye, Vec3::NEG_Z, FAR).expect("straight at it");
        assert!((hit - 7.5).abs() < 1e-3, "entered at {hit}");
        // A metre to the side of a half-metre box is a miss.
        assert!(crate_.cast_ray(eye + Vec3::X, Vec3::NEG_Z, FAR).is_none());
    }

    /// The point of holding a collider rather than an enum: a shape nothing was written for.
    ///
    /// A cylinder is neither a capsule nor a box, and adding it took no change to this file. That
    /// is the property a vehicle needs.
    #[test]
    fn a_shape_nobody_wrote_a_case_for_is_a_target() {
        let barrel = Hitbox::new(Collider::cylinder(0.4, 2.0), Vec3::new(0.0, 1.0, -6.0), Quat::IDENTITY);
        let eye = Vec3::new(0.0, 1.0, 0.0);
        let hit = barrel.cast_ray(eye, Vec3::NEG_Z, FAR).expect("straight at it");
        // Radius 0.4, so the near face of a barrel centred at -6 is at -5.6.
        assert!((hit - 5.6).abs() < 1e-2, "entered at {hit}");
    }

    /// A target that turned between two updates is rewound to the angle it was at, not to either
    /// end. The enum could not express this at all: it had no rotation.
    #[test]
    fn a_turn_is_rewound_part_way() {
        // A plank four metres long and a fifth of a metre thick, centred six metres ahead. Face on,
        // a shot down the middle meets its thin side; turned a quarter, it meets the long one, two
        // metres nearer.
        let plank = |angle: f32| {
            Hitbox::new(
                Collider::cuboid(4.0, 2.0, 0.2),
                Vec3::new(0.0, 1.0, -6.0),
                Quat::from_rotation_y(angle),
            )
        };
        let eye = Vec3::new(0.0, 1.0, 0.0);
        let range = |hitbox: Hitbox| hitbox.cast_ray(eye, Vec3::NEG_Z, FAR).expect("straight at it");

        let flat = range(plank(0.0));
        let turned = range(plank(core::f32::consts::FRAC_PI_2));
        assert!(flat > turned + 1.0, "the turn made no difference: {flat} against {turned}");

        let rewound = range(Hitbox::lerp(&plank(0.0), &plank(core::f32::consts::FRAC_PI_2), 1.0));
        assert!(
            (rewound - turned).abs() < 1e-3,
            "the rewind ignored the turn: stopped at {rewound}, not {turned}"
        );
    }

    /// A box that moved between two updates is drawn, and rewound, in between them.
    #[test]
    fn a_pose_blends_but_a_shape_does_not() {
        let start = box_at(Vec3::new(0.0, 2.0, 0.0), Vec3::splat(0.5));
        let end = box_at(Vec3::new(0.0, 4.0, 0.0), Vec3::splat(9.0));
        let middle = Hitbox::lerp(&start, &end, 0.25);
        assert_eq!(middle.position, Vec3::new(0.0, 2.5, 0.0));

        // The shape held is the older one, at its own size: a ray that grazes past the small box
        // must still miss, however large the box it is turning into.
        let past_the_corner = Vec3::new(0.7, 2.5, 10.0);
        assert!(
            middle.cast_ray(past_the_corner, Vec3::NEG_Z, FAR).is_none(),
            "the blend grew into the shape it had not reached yet"
        );
    }

    #[test]
    fn a_stance_holds_until_the_moment_arrives() {
        let start = player(Vec3::ZERO, true);
        let end = player(Vec3::X, false);
        let middle = Hitbox::lerp(&start, &end, 0.9);

        // The crouched capsule sits lower, so its centre is below a standing one's...
        assert!(middle.position.y < end.position.y, "the stance's height was blended");
        // ...and a shot at standing head height still misses.
        let eye = Vec3::new(0.9, EYE_HEIGHT, 10.0);
        assert!(
            middle.cast_ray(eye, Vec3::NEG_Z, FAR).is_none(),
            "stance changed before reaching the sample"
        );
        let _ = CAPSULE_RADIUS;
    }
}
