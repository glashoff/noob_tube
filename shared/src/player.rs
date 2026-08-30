//! Player state and the input step that advances it.
//!
//! Ported from `Pawn.applyInput` in `webgame/shared/game/Pawn.ts`. Both the server and the
//! client's prediction replay call [`PlayerState::apply_input`], so it must stay deterministic:
//! same state plus same input yields the same result, or prediction and authority drift apart.

use bevy::math::{Vec2, Vec3};

use crate::collision::CollisionWorld;
use crate::movement::*;

/// One tick's worth of player intent.
///
/// Only the fields here may influence movement. Anything read from elsewhere — wall-clock time,
/// frame rate, randomness — would break replay.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PlayerInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub crouch: bool,
    /// Horizontal look angle in radians. Movement is relative to it.
    pub yaw: f32,
    /// Vertical look angle in radians. Does not affect movement, but travels with the input so the
    /// server knows where a shot was aimed.
    pub pitch: f32,
}

impl PlayerInput {
    /// Movement intent in local space: +Y forward, +X right, before yaw is applied.
    fn local_direction(&self) -> Vec2 {
        let x = (self.right as i32 - self.left as i32) as f32;
        let y = (self.forward as i32 - self.backward as i32) as f32;
        let dir = Vec2::new(x, y);
        // Normalising keeps diagonals from being faster than the cardinals.
        dir.normalize_or_zero()
    }
}

/// Everything about a player that movement depends on.
///
/// This is the complete rollback snapshot — four fields. Keeping it this small is the point of
/// having no solver state anywhere.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerState {
    /// Position of the feet, not the capsule centre.
    pub position: Vec3,
    pub velocity: Vec3,
    pub on_ground: bool,
    pub crouching: bool,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, FLOOR_Y, 0.0),
            velocity: Vec3::ZERO,
            on_ground: true,
            crouching: false,
        }
    }
}

impl PlayerState {
    /// Camera height above the feet for the current stance.
    pub fn eye_height(&self) -> f32 {
        if self.crouching {
            CROUCH_EYE_HEIGHT
        } else {
            EYE_HEIGHT
        }
    }

    pub fn eye_position(&self) -> Vec3 {
        self.position + Vec3::Y * self.eye_height()
    }

    /// Advances one tick.
    pub fn apply_input(&mut self, input: &PlayerInput, world: &CollisionWorld, dt: f32) {
        self.update_stance(input, world);

        let speed = if self.crouching {
            CROUCH_SPEED
        } else if self.on_ground {
            MAX_SPEED
        } else {
            MAX_SPEED_AIR
        };

        // Rotate the intent into world space and ramp toward it rather than snapping, so starting,
        // stopping and reversing ease in and out.
        let local = input.local_direction();
        // At yaw 0 forward is -Z and right is +X, matching Bevy's convention.
        let (sin, cos) = input.yaw.sin_cos();
        let target = Vec3::new(
            local.x * cos - local.y * sin,
            0.0,
            -local.y * cos - local.x * sin,
        ) * speed;

        let current = Vec3::new(self.velocity.x, 0.0, self.velocity.z);
        let step = MOVE_ACCEL * dt;
        let horizontal = if current.distance(target) <= step {
            target
        } else {
            current + (target - current).normalize_or_zero() * step
        };
        self.velocity.x = horizontal.x;
        self.velocity.z = horizontal.z;

        if input.jump && self.on_ground {
            self.velocity.y = JUMP_VELOCITY;
            self.on_ground = false;
        } else if self.on_ground {
            self.velocity.y = -GROUND_STICK_SPEED;
        } else {
            self.velocity.y += GRAVITY * dt;
        }

        let wanted = self.velocity * dt;
        let moved = world.sweep_capsule(self.position, wanted, self.crouching);
        self.position += moved;

        // Where the sweep refused to take us, the velocity in that direction is spent — otherwise
        // gravity would accumulate forever while standing on the floor, and the first step off a
        // ledge would launch the player downward.
        if wanted.y < 0.0 && moved.y > wanted.y + 1e-6 {
            self.velocity.y = 0.0;
        } else if wanted.y > 0.0 && moved.y < wanted.y - 1e-6 {
            self.velocity.y = 0.0;
        }

        // The ground probe reaches GROUND_SNAP_DIST below the feet, and one tick into a jump the
        // player has not cleared that yet. Counting that as grounded would let a held jump key
        // re-trigger every tick, pinning the player just above the floor.
        self.on_ground = self.velocity.y <= 0.0 && world.is_grounded(self.position, self.crouching);

        // Hold the capsule one skin above the surface while grounded, never on it and never in
        // it. The sweep leaves it a fraction of a millimetre low each tick and never puts that
        // back, which compounds into centimetres over a minute of walking; but resting it exactly
        // on the surface is worse, because a shape cast that starts touching its target reports
        // contact at distance zero and the slide loop makes no progress at all. A whole skin of
        // clearance keeps every cast in the well-behaved regime.
        if self.on_ground
            && let Some(ground) = world.ground_height_below(self.position)
            && (self.position.y - ground).abs() < GROUND_SNAP_DIST
        {
            self.position.y = ground + SKIN;
        }

    }

    /// Crouching starts the moment the key is held; standing back up has to wait for headroom.
    fn update_stance(&mut self, input: &PlayerInput, world: &CollisionWorld) {
        self.crouching = if input.crouch {
            true
        } else {
            !world.can_stand_up(self.position) && self.crouching
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floor_world() -> CollisionWorld {
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

    const DT: f32 = 1.0 / 64.0;

    fn run(state: &mut PlayerState, input: &PlayerInput, world: &CollisionWorld, ticks: usize) {
        for _ in 0..ticks {
            state.apply_input(input, world, DT);
        }
    }

    #[test]
    fn standing_still_stays_on_the_floor() {
        let world = floor_world();
        let mut state = PlayerState::default();
        run(&mut state, &PlayerInput::default(), &world, 64);
        assert!(state.on_ground);
        assert!(state.position.y.abs() < 0.05, "{:?}", state.position);
        // Gravity must not accumulate while grounded.
        assert!(state.velocity.y.abs() < 1e-3, "{}", state.velocity.y);
    }

    #[test]
    fn walking_forward_reaches_max_speed() {
        let world = floor_world();
        let mut state = PlayerState::default();
        let input = PlayerInput { forward: true, ..default_input() };
        // MOVE_ACCEL_TIME is 0.3 s; a full second is comfortably enough.
        run(&mut state, &input, &world, 64);
        let speed = Vec3::new(state.velocity.x, 0.0, state.velocity.z).length();
        assert!((speed - MAX_SPEED).abs() < 0.01, "speed was {speed}");
        // Yaw 0 means forward is -Z, matching Bevy's convention.
        assert!(state.position.z < -1.0, "{:?}", state.position);
    }

    #[test]
    fn diagonals_are_not_faster() {
        let world = floor_world();
        let mut state = PlayerState::default();
        let input = PlayerInput { forward: true, right: true, ..default_input() };
        run(&mut state, &input, &world, 64);
        let speed = Vec3::new(state.velocity.x, 0.0, state.velocity.z).length();
        assert!((speed - MAX_SPEED).abs() < 0.01, "diagonal speed was {speed}");
    }

    #[test]
    fn jumping_leaves_the_ground_and_lands_again() {
        let world = floor_world();
        let mut state = PlayerState::default();
        let jump = PlayerInput { jump: true, ..default_input() };

        state.apply_input(&jump, &world, DT);
        assert!(!state.on_ground, "did not leave the ground");
        assert!(state.position.y > 0.0);

        // JUMP_VELOCITY 6.5 against GRAVITY -20 gives roughly 0.65 s of flight.
        run(&mut state, &default_input(), &world, 64);
        assert!(state.on_ground, "never landed");
        assert!(state.position.y.abs() < 0.05, "{:?}", state.position);
    }

    #[test]
    fn crouching_slows_the_player_down() {
        let world = floor_world();
        let mut state = PlayerState::default();
        let input = PlayerInput { forward: true, crouch: true, ..default_input() };
        run(&mut state, &input, &world, 64);
        assert!(state.crouching);
        let speed = Vec3::new(state.velocity.x, 0.0, state.velocity.z).length();
        assert!((speed - CROUCH_SPEED).abs() < 0.01, "crouch speed was {speed}");
        assert_eq!(state.eye_height(), CROUCH_EYE_HEIGHT);
    }

    /// Standing still must not sink. The capsule settles a fraction of a tick's gravity into the
    /// floor on the first step — the cast finds no overlap when it starts exactly on the surface —
    /// but that must be a one-off, not a slow descent through the level.
    #[test]
    fn standing_does_not_drift_downward() {
        let world = floor_world();
        let mut state = PlayerState::default();

        run(&mut state, &PlayerInput::default(), &world, 64);
        let after_one_second = state.position.y;

        run(&mut state, &PlayerInput::default(), &world, 64 * 60);
        let after_a_minute = state.position.y;

        assert!(
            (after_a_minute - after_one_second).abs() < 1e-6,
            "sank from {after_one_second} to {after_a_minute} over a minute"
        );
        assert!(after_a_minute > -0.01, "settled too deep: {after_a_minute}");
    }

    /// Walking across the floor must not sink either — the sweep runs a longer path each tick.
    #[test]
    fn walking_does_not_drift_downward() {
        let world = floor_world();
        let mut state = PlayerState::default();
        let input = PlayerInput { forward: true, ..default_input() };

        run(&mut state, &input, &world, 64);
        let early = state.position.y;
        run(&mut state, &input, &world, 64 * 30);
        let late = state.position.y;

        assert!((late - early).abs() < 1e-6, "sank from {early} to {late} while walking");
    }

    /// Walking has to keep working, not just start working. The drift tests above only ever
    /// checked height, which is how a total stall went unnoticed.
    #[test]
    fn walking_keeps_covering_ground() {
        let world = floor_world();
        let mut state = PlayerState::default();
        let input = PlayerInput { forward: true, ..default_input() };

        run(&mut state, &input, &world, 64 * 10);
        let after_ten_seconds = -state.position.z;

        // Ten seconds at 5.5 m/s, less the acceleration ramp, is comfortably over 50 m.
        assert!(
            after_ten_seconds > 50.0,
            "only covered {after_ten_seconds} m in ten seconds"
        );
    }

    /// The whole point of keeping this deterministic.
    #[test]
    fn identical_runs_produce_identical_state() {
        let world = floor_world();
        let input = PlayerInput { forward: true, right: true, jump: true, yaw: 0.7, ..default_input() };

        let mut a = PlayerState::default();
        let mut b = PlayerState::default();
        run(&mut a, &input, &world, 40);
        run(&mut b, &input, &world, 40);
        assert_eq!(a, b);
    }

    fn default_input() -> PlayerInput {
        PlayerInput::default()
    }
}
