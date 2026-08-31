//! What a shot can hit.
//!
//! Everything the server tests a shot against is one of these, players and props alike. Before this
//! existed a target was a feet position and a `crouching` flag, which is a player and nothing else —
//! the shape was hard-coded in the hit test and could not be anything but the movement capsule.
//!
//! The shape travels *with* the pose rather than sitting on the entity, because it changes over
//! time and the whole point of rewinding is to test against the shape that was there then. Crouching
//! was already an example of that before any prop existed: someone who ducked half a round trip ago
//! must still be standing in the past the shooter aimed at. A door that swings, a crate that grows
//! a dent — all the same case.

use bevy::prelude::*;
use rapier3d::parry::query::RayCast;
use rapier3d::prelude::*;
use serde::{Deserialize, Serialize};

use crate::movement::{
    CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET, CROUCH_CAPSULE_HALF_HEIGHT,
    CROUCH_CAPSULE_Y_OFFSET,
};
use crate::player::PlayerState;

/// A shape at a place, for one tick.
///
/// A component as well as a value: a prop carries its own and has it replicated, while a player's is
/// derived from `PlayerState` because that is already the authority on where a player is and how
/// they are standing. Two ways to arrive at the same thing, which is what keeps `PlayerState` from
/// growing a redundant copy of its own shape.
#[derive(Component, Clone, Copy, Debug, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub enum Hitbox {
    /// The movement capsule, given by the feet, in one of its two heights.
    Player { feet: Vec3, crouching: bool },
    /// An axis-aligned box, given by its centre.
    Prop { centre: Vec3, half_extents: Vec3 },
}

impl Hitbox {
    /// The capsule a player presents right now.
    pub fn of(state: &PlayerState) -> Self {
        Hitbox::Player {
            feet: state.position,
            crouching: state.crouching,
        }
    }

    /// Where the shape is, for saying how far it has moved.
    pub fn centre(&self) -> Vec3 {
        match *self {
            Hitbox::Player { feet, crouching } => feet + Vec3::Y * capsule(crouching).1,
            Hitbox::Prop { centre, .. } => centre,
        }
    }

    /// Distance along the ray at which it enters this shape, if it does.
    pub fn cast_ray(&self, origin: Vec3, direction: Vec3, max_distance: f32) -> Option<f32> {
        let ray = Ray::new(origin.into(), direction.into());
        // `solid` so a shot fired from inside a shape counts as an immediate hit rather than
        // passing through and striking the far wall of it.
        match *self {
            Hitbox::Player { feet, crouching } => {
                let (half_height, y_offset) = capsule(crouching);
                let shape = Capsule::new_y(half_height, CAPSULE_RADIUS);
                shape.cast_ray(
                    &Pose::from_translation(feet + Vec3::Y * y_offset),
                    &ray,
                    max_distance,
                    true,
                )
            }
            Hitbox::Prop {
                centre,
                half_extents,
            } => {
                // Spelled out because `bevy::prelude` has a `Cuboid` of its own, which is a
                // mesh primitive and cannot cast a ray.
                let shape = rapier3d::parry::shape::Cuboid::new(half_extents);
                shape.cast_ray(&Pose::from_translation(centre), &ray, max_distance, true)
            }
        }
    }

    /// Blends two moments of the same shape.
    ///
    /// Used both by a client drawing a prop between two received updates and by the server rebuilding
    /// that same blend to rewind it. Only the *place* is blended: stance and extents are discrete,
    /// and half a crouch is not a stance. Two different shapes cannot be blended at all — that is a
    /// prop turning into a player, which nothing does — so the older one stands.
    pub fn lerp(start: Self, end: Self, t: f32) -> Self {
        match (start, end) {
            (
                Hitbox::Player { feet, crouching },
                Hitbox::Player {
                    feet: end_feet, ..
                },
            ) => Hitbox::Player {
                feet: feet.lerp(end_feet, t),
                crouching,
            },
            (
                Hitbox::Prop {
                    centre,
                    half_extents,
                },
                Hitbox::Prop {
                    centre: end_centre, ..
                },
            ) => Hitbox::Prop {
                centre: centre.lerp(end_centre, t),
                half_extents,
            },
            _ => start,
        }
    }
}

/// Half-height and centre offset of the capsule for a stance.
fn capsule(crouching: bool) -> (f32, f32) {
    if crouching {
        (CROUCH_CAPSULE_HALF_HEIGHT, CROUCH_CAPSULE_Y_OFFSET)
    } else {
        (CAPSULE_HALF_HEIGHT, CAPSULE_Y_OFFSET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAR: f32 = 200.0;

    fn standing(feet: Vec3) -> Hitbox {
        Hitbox::Player {
            feet,
            crouching: false,
        }
    }

    #[test]
    fn a_shot_down_the_middle_enters_the_capsule() {
        let hit = standing(Vec3::new(0.0, 0.0, -10.0))
            .cast_ray(Vec3::new(0.0, 1.59, 0.0), Vec3::NEG_Z, FAR)
            .expect("a target straight ahead was not hit");
        assert!(hit > 9.0 && hit < 10.0, "entered at {hit}");
    }

    /// A crouched player is a smaller target, which is the point of crouching.
    #[test]
    fn crouching_ducks_under_a_level_shot() {
        let feet = Vec3::new(0.0, 0.0, -10.0);
        let eye = Vec3::new(0.0, 1.59, 0.0);
        assert!(standing(feet).cast_ray(eye, Vec3::NEG_Z, FAR).is_some());
        assert!(
            Hitbox::Player { feet, crouching: true }
                .cast_ray(eye, Vec3::NEG_Z, FAR)
                .is_none(),
            "a crouched player was hit by a shot at standing head height"
        );
    }

    /// The whole reason this type exists: something that is not a player.
    #[test]
    fn a_box_is_hit_on_its_near_face() {
        let crate_ = Hitbox::Prop {
            centre: Vec3::new(0.0, 3.0, -8.0),
            half_extents: Vec3::splat(0.5),
        };
        let eye = Vec3::new(0.0, 3.0, 0.0);
        let hit = crate_.cast_ray(eye, Vec3::NEG_Z, FAR).expect("straight at it");
        assert!((hit - 7.5).abs() < 1e-3, "entered at {hit}");
        // A metre to the side of a half-metre box is a miss.
        assert!(crate_.cast_ray(eye + Vec3::X, Vec3::NEG_Z, FAR).is_none());
    }

    /// A box that moved between two updates is drawn, and rewound, in between them.
    #[test]
    fn a_prop_blends_its_place_but_not_its_size() {
        let start = Hitbox::Prop {
            centre: Vec3::new(0.0, 2.0, 0.0),
            half_extents: Vec3::splat(0.5),
        };
        let end = Hitbox::Prop {
            centre: Vec3::new(0.0, 4.0, 0.0),
            half_extents: Vec3::splat(9.0),
        };
        let Hitbox::Prop { centre, half_extents } = Hitbox::lerp(start, end, 0.25) else {
            panic!("a prop blended into something else");
        };
        assert_eq!(centre, Vec3::new(0.0, 2.5, 0.0));
        assert_eq!(half_extents, Vec3::splat(0.5), "extents were blended");
    }

    #[test]
    fn a_stance_holds_until_the_moment_arrives() {
        let start = Hitbox::Player { feet: Vec3::ZERO, crouching: true };
        let end = Hitbox::Player { feet: Vec3::X, crouching: false };
        let Hitbox::Player { feet, crouching } = Hitbox::lerp(start, end, 0.9) else {
            panic!("a player blended into something else");
        };
        assert_eq!(feet, Vec3::new(0.9, 0.0, 0.0));
        assert!(crouching, "stance changed before reaching the sample");
    }
}
