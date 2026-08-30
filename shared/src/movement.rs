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

/// Gap kept between the capsule and surfaces so it does not stick to them.
pub const SKIN: f32 = 0.01;
/// How far below the feet to look for ground.
pub const GROUND_SNAP_DIST: f32 = 0.12;

/// Camera height above the feet, standing and crouched. Derived from the posed model in `webgame`:
/// the eyes sit about 40% up from the Head joint toward HeadTop.
pub const EYE_HEIGHT: f32 = 1.59;
pub const CROUCH_EYE_HEIGHT: f32 = 1.10;
