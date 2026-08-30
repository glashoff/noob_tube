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

/// The placeholder silhouette: a body capsule with a head box on top.
///
/// Both must fit **inside** the collision capsule, because that capsule is the hitbox — a shot is
/// tested against it and against nothing else. A silhouette that reached outside would let a player
/// aim at a head no raycast can reach, which is exactly what the first version of these heads did:
/// it sat from 1.70 m to 2.04 m, entirely above the 1.70 m capsule.
///
/// The body is drawn shorter than the collision capsule on purpose. Drawn at full height it would
/// enclose the head and hide it, which is the conflict that produced the broken version — the fix
/// is to give the head room rather than to move it out of the way.
///
/// `silhouette_fits_inside_the_hitbox` in this module holds the containment to account.
pub const BODY_RADIUS: f32 = 0.30;
pub const BODY_HALF_HEIGHT: f32 = 0.53;
/// Body capsule centre above the feet. Its top lands at `BODY_HEIGHT`, where the head starts.
pub const BODY_Y_OFFSET: f32 = BODY_HALF_HEIGHT + BODY_RADIUS;
pub const HEAD_SIZE: f32 = 0.28;
/// Head centre above the feet.
pub const HEAD_Y: f32 = 1.48;

/// Gap kept between the capsule and surfaces so it does not stick to them.
pub const SKIN: f32 = 0.01;
/// How far below the feet to look for ground.
pub const GROUND_SNAP_DIST: f32 = 0.12;

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

    /// The hitbox is the collision capsule and nothing else, so anything a player can see has to be
    /// inside it. Otherwise there is a part of the silhouette that cannot be shot, which is worse
    /// than an invisible one — the player aims at it and is told they missed.
    #[test]
    fn silhouette_fits_inside_the_hitbox() {
        // Worst case is a top corner of the head box: furthest out horizontally *and* highest.
        let half = HEAD_SIZE / 2.0;
        let corner_distance = (half * half * 2.0f32).sqrt();
        assert!(
            inside_capsule(corner_distance, HEAD_Y + half),
            "the head's top corners reach outside the hitbox"
        );

        // And the body's own widest, highest ring.
        let body_top = BODY_Y_OFFSET + BODY_HALF_HEIGHT;
        assert!(
            inside_capsule(BODY_RADIUS, body_top),
            "the body capsule's shoulder reaches outside the hitbox"
        );
        assert!(
            inside_capsule(BODY_RADIUS, BODY_Y_OFFSET - BODY_HALF_HEIGHT),
            "the body capsule's hip reaches outside the hitbox"
        );

        // The head has to start below where the body ends, or there is a gap to see through. They
        // overlap by a couple of centimetres, as body and head do on any real model.
        assert!(
            HEAD_Y - half < body_top,
            "a gap between body and head: body ends at {body_top}, head starts at {}",
            HEAD_Y - half
        );
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
