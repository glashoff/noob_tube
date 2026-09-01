//! Movement constants, taken verbatim from the `webgame` project so the two games feel identical.
//!
//! Sources: `webgame/shared/physics/movement.ts` and `webgame/shared/game/physics.ts`.

/// Ground plane height.
pub const FLOOR_Y: f32 = 0.0;

/// Horizontal speed on the ground.
pub const MAX_SPEED: f32 = 5.5;
/// Horizontal speed while airborne.
pub const MAX_SPEED_AIR: f32 = 3.0;
/// Horizontal speed while crouching.
pub const CROUCH_SPEED: f32 = 2.6;

pub const GRAVITY: f32 = -20.0;
pub const JUMP_VELOCITY: f32 = 6.5;

/// Horizontal movement ramps toward its target velocity rather than snapping, so starting,
/// stopping and reversing ease in and out. The rate is chosen so a full-speed change takes
/// `MOVE_ACCEL_TIME` seconds. Applied identically on the server and in the client's prediction
/// replay, or velocity would diverge between them.
pub const MOVE_ACCEL_TIME: f32 = 0.3;
pub const MOVE_ACCEL: f32 = MAX_SPEED / MOVE_ACCEL_TIME;

/// Collision capsule: upright, Y-aligned, a cylinder of `CAPSULE_HALF_HEIGHT` with hemispherical
/// caps of `CAPSULE_RADIUS`. Total height 1.7 m standing, 1.2 m crouched.
pub const CAPSULE_RADIUS: f32 = 0.35;
pub const CAPSULE_HALF_HEIGHT: f32 = 0.5;
pub const CROUCH_CAPSULE_HALF_HEIGHT: f32 = 0.25;

/// Capsule centre above the entity's feet.
pub const CAPSULE_Y_OFFSET: f32 = CAPSULE_HALF_HEIGHT + CAPSULE_RADIUS;
pub const CROUCH_CAPSULE_Y_OFFSET: f32 = CROUCH_CAPSULE_HALF_HEIGHT + CAPSULE_RADIUS;

/// How tall a standing player is, and so how tall the drawn figure may be.
///
/// The collision capsule *is* the hitbox — a shot is tested against it and against nothing else —
/// so anything drawn above this is a part of a player that can be aimed at and never hit. That
/// mistake has been made here once, with a head box that sat from 1.70 m to 2.04 m: perfectly
/// visible and impossible to shoot. `client/src/character.rs` scales the character model by this.
pub const CAPSULE_HEIGHT: f32 = 2.0 * (CAPSULE_HALF_HEIGHT + CAPSULE_RADIUS);

/// Gap kept between the capsule and surfaces so it does not stick to them.
pub const SKIN: f32 = 0.01;
/// How far below the feet to look for ground.
pub const GROUND_SNAP_DIST: f32 = 0.12;

/// How steep ground may be and still be stood on: the smallest upward component its normal may
/// have. 0.7 is a slope of about 45.6°.
///
/// This is an authoring decision before it is a physical one — it is the line between a hillside
/// and a cliff, and so it decides what a map can be walled in with. Taken from `webgame`, which
/// settled on the same number.
///
/// Before terrain there was nothing to test: every walkable surface was horizontal and every wall
/// vertical, so a downward ray that hit anything had hit a floor by construction. That stopped
/// being true the moment the ground had hills in it.
pub const WALKABLE_NORMAL_Y: f32 = 0.7;

/// How far a resting capsule's feet float above the surface, because the capsule is round and the
/// surface is not level.
///
/// A sphere of radius `r` resting on a plane whose normal has upward component `n` has its centre
/// `r / n` above that plane *vertically*, so the point directly below the centre — what this code
/// calls the feet — never touches the ground on a slope. On flat ground this is zero, and at the
/// limit it is 15 cm, which is more than [`GROUND_SNAP_DIST`]: without the correction the ground
/// probe loses the floor at about 41° and the slope limit is decided by capsule geometry instead
/// of by the number above.
///
/// Only ever evaluated for walkable ground, which is what keeps `1.0 / n` bounded.
pub fn slope_lift(normal_y: f32) -> f32 {
    CAPSULE_RADIUS * (1.0 / normal_y - 1.0)
}

/// The most [`slope_lift`] can return, and so how far below the feet the ground probe has to reach.
pub const MAX_SLOPE_LIFT: f32 = CAPSULE_RADIUS * (1.0 / WALKABLE_NORMAL_Y - 1.0);

/// Downward speed applied while grounded, instead of gravity.
///
/// Gravity accumulates: each tick it drives the capsule further into the floor for the sweep to
/// cancel, and the fraction the sweep fails to cancel adds up until the player has sunk
/// centimetres into the ground. A constant bias cannot accumulate, still holds the capsule against
/// the floor, and pulls it down the last few centimetres after a landing — `is_grounded` reaches
/// GROUND_SNAP_DIST, so without it the player would hover wherever the ground probe first caught.
pub const GROUND_STICK_SPEED: f32 = 2.0;

/// Camera height above the feet, standing and crouched. Derived from the posed model in `webgame`:
/// the eyes sit about 40% up from the Head joint toward HeadTop.
pub const EYE_HEIGHT: f32 = 1.59;
pub const CROUCH_EYE_HEIGHT: f32 = 1.10;


#[cfg(test)]
mod tests {
    use super::*;

    /// [`CAPSULE_HEIGHT`] has to be the height of the capsule, because the client scales the
    /// character model by it and a model scaled by a wrong number is a player with an unhittable
    /// head. Derived rather than typed, so this guards a hand edit rather than arithmetic.
    #[test]
    fn the_stated_height_is_the_capsule_it_describes() {
        assert!(
            inside_capsule(0.0, CAPSULE_HEIGHT - 1e-4),
            "the top of the capsule is below the height claimed for it",
        );
        assert!(
            !inside_capsule(0.0, CAPSULE_HEIGHT + 1e-3),
            "the capsule reaches above the height claimed for it",
        );
    }

    /// And the camera has to look out from inside it. A viewpoint above a player's own hitbox is a
    /// player who can see over cover that no shot of theirs — or at them — can cross.
    #[test]
    fn the_eye_looks_out_from_inside_the_hitbox() {
        assert!(inside_capsule(0.0, EYE_HEIGHT), "the standing eye is outside the hitbox");
        assert!(inside_capsule(0.0, CROUCH_EYE_HEIGHT), "the crouched eye is outside the hitbox");
    }

    /// Is a point at horizontal distance `d` and height `y` above the feet inside the standing
    /// collision capsule?
    fn inside_capsule(d: f32, y: f32) -> bool {
        let cap_centre = if y > CAPSULE_Y_OFFSET {
            // Upper hemisphere.
            CAPSULE_Y_OFFSET + CAPSULE_HALF_HEIGHT
        } else if y < CAPSULE_Y_OFFSET {
            CAPSULE_Y_OFFSET - CAPSULE_HALF_HEIGHT
        } else {
            y
        };
        // Inside the cylindrical middle, only the radius matters.
        if (y - CAPSULE_Y_OFFSET).abs() <= CAPSULE_HALF_HEIGHT {
            return d <= CAPSULE_RADIUS;
        }
        let dy = y - cap_centre;
        d * d + dy * dy <= CAPSULE_RADIUS * CAPSULE_RADIUS
    }
}
