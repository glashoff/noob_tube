//! Player state and the input step that advances it.
//!
//! Ported from `Pawn.applyInput` in `webgame/shared/game/Pawn.ts`. Both the server and the
//! client's prediction replay call [`PlayerState::apply_input`], so it must stay deterministic:
//! same state plus same input yields the same result, or prediction and authority drift apart.

use bevy::ecs::entity::{EntityMapper, MapEntities};
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::physics::Level;
use crate::movement::*;

/// One tick's worth of player intent.
///
/// Only the fields here may influence movement. Anything read from elsewhere — wall-clock time,
/// frame rate, randomness — would break replay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Reflect, Serialize, Deserialize)]
pub struct PlayerInput {
    pub forward: bool,
    pub backward: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    pub crouch: bool,
    /// Held, not tapped. The rate of fire comes from [`PlayerState::fire_cooldown`], so holding the
    /// button produces a steady stream rather than one shot per tick.
    pub fire: bool,
    /// Get in, or get out. Held, like everything else here — the *edge* is found by the server,
    /// which compares this tick's input with the last one it acted on.
    ///
    /// It has to travel as an input rather than as a message, because the moment it happens is a
    /// tick and every other decision on this struct is made at one. A message would arrive between
    /// two ticks and there would be no honest answer to which of them it belonged to.
    pub interact: bool,
    /// Asking to be put back on the wheels, held rather than tapped: the vehicle counts the ticks
    /// and acts once they add up to [`righting_hold`](crate::vehicle::VehicleSpec::righting_hold).
    ///
    /// The same button as [`fire`](Self::fire), and a field of its own all the same, because the
    /// two are suppressed by different rules — a driver whose gun is stowed or cannot bear is not
    /// firing, and is exactly the driver most likely to be upside down.
    pub righting: bool,
    /// Asking for the vehicle to be picked up and set down again, level and clear of the ground.
    ///
    /// The blunt instrument, and a separate button from [`righting`](Self::righting) because it is
    /// a separate thing to ask for. Righting leans on the vehicle until it comes up, which needs
    /// room to come up in; this one does not care what the vehicle is wedged against. A driver
    /// stuck on the wall of a ravine has been asking for the wrong one.
    ///
    /// Held like everything else here, and the vehicle's own cooldown is what turns holding it into
    /// one rescue rather than flight by teleport.
    pub recover: bool,
    /// Held rather than tapped, and latched by the client rather than found here.
    ///
    /// Flight is a mode, and a mode found from an *edge* would need last tick's input to find it —
    /// which a replayed tick does not have and which a dropped packet would lose. So the client
    /// holds the toggle and says on every tick which mode it is in, and this step simply obeys.
    /// That is the same shape [`crouch`](Self::crouch) already has, and it is what makes flight
    /// survive a rollback: replaying the tick replays the mode.
    pub flying: bool,
    /// Horizontal look angle in radians. Movement is relative to it.
    pub yaw: f32,
    /// Vertical look angle in radians. Does not affect movement, but travels with the input so the
    /// server knows where a shot was aimed.
    pub pitch: f32,
    /// What the screen was showing of everyone else, at the moment the trigger went down.
    ///
    /// `None` on every tick that is not a shot, which costs nothing on the wire: lightyear sends
    /// "same as the previous tick" for an unchanged input, and an input that carries no bracket is
    /// unchanged whenever the keys are.
    pub view: Option<ViewBracket>,
}

/// The two received snapshots a client was drawing between, and how far between them it was.
///
/// This is the exact answer to "what did the shooter see". Remote players are drawn interpolated,
/// which means their position on screen was never a position the server simulated — it was a blend
/// of two, at some fraction. Sending the fraction and both ends of it lets the server rebuild that
/// blend from its own history and get the identical point back, rather than approximating it from
/// a delay in milliseconds.
///
/// The ticks are the *confirmed* ticks — the ones replication actually delivered, which at a send
/// rate below the tick rate are further apart than one tick. That is why both are needed and a
/// single instant would not do.
#[derive(Clone, Copy, Debug, PartialEq, Reflect, Serialize, Deserialize)]
pub struct ViewBracket {
    /// The older of the two snapshots.
    pub from: lightyear::prelude::Tick,
    /// The newer one. Always strictly after `from`.
    pub to: lightyear::prelude::Tick,
    /// 0 at `from`, 1 at `to`.
    pub factor: f32,
}

/// Required by lightyear's input plugin. Our input carries no entity references — it is booleans
/// and two angles — so there is nothing to remap when entity ids differ between peers.
impl MapEntities for PlayerInput {
    fn map_entities<M: EntityMapper>(&mut self, _mapper: &mut M) {}
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
///
/// It is deliberately *not* everything that gets replicated about a player. Where the player is
/// looking is replicated too, but as a separate [`Aim`] component, because the two need opposite
/// treatment: the server is the authority on position, while for the local player's own aim the
/// client is. Rolling `Aim` back on the local player would make the view snap every time a server
/// packet arrived. Lightyear configures prediction and interpolation per component, so keeping them
/// apart is what makes that possible at all.
#[derive(Component, Clone, Copy, Debug, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct PlayerState {
    /// Position of the feet, not the capsule centre.
    pub position: Vec3,
    pub velocity: Vec3,
    pub on_ground: bool,
    pub crouching: bool,
    /// Ticks until this player may fire again.
    ///
    /// In the rollback snapshot on purpose. It is the client's own answer to "can I shoot yet",
    /// and it has to be predicted for the weapon to feel connected to the trigger; leaving it to
    /// the server would put a round trip between the click and the shot.
    pub fire_cooldown: u8,
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, FLOOR_Y, 0.0),
            velocity: Vec3::ZERO,
            on_ground: true,
            crouching: false,
            fire_cooldown: 0,
        }
    }
}

/// Where a player is looking.
///
/// Separate from [`PlayerState`] on purpose — see the note there. The server learns it from
/// [`PlayerInput`] and replicates it so other clients can aim a body and a head.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Aim {
    pub yaw: f32,
    pub pitch: f32,
}

/// Identifies which connected peer a player entity belongs to.
///
/// Replicated, so a client can tell its own player from everyone else's.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Player {
    pub peer: u64,
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

    /// True when the trigger is down and the weapon is ready.
    ///
    /// Read *before* [`apply_input`](Self::apply_input) consumes it: that call starts the cooldown,
    /// so afterwards the answer is always no.
    pub fn is_firing(&self, input: &PlayerInput) -> bool {
        input.fire && self.fire_cooldown == 0
    }

    /// Advances one tick.
    pub fn apply_input(&mut self, input: &PlayerInput, level: &Level, dt: f32) {
        // Before the stance changes, so a shot uses the stance it was aimed from.
        if self.is_firing(input) {
            self.fire_cooldown = crate::shooting::FIRE_INTERVAL_TICKS;
        } else {
            self.fire_cooldown = self.fire_cooldown.saturating_sub(1);
        }

        if input.flying {
            self.fly(input, level, dt);
            return;
        }

        self.update_stance(input, level);

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
        let moved = level.sweep_capsule(self.position, wanted, self.crouching);
        self.position += moved;

        // The ground probe reaches GROUND_SNAP_DIST below the feet, and one tick into a jump the
        // player has not cleared that yet. Counting that as grounded would let a held jump key
        // re-trigger every tick, pinning the player just above the floor.
        let footing = level.footing_below(self.position);
        self.on_ground = self.velocity.y <= 0.0 && footing.is_some();

        // Where the sweep refused to take us, the velocity in that direction is spent — otherwise
        // gravity would accumulate forever while standing on the floor, and the first step off a
        // ledge would launch the player downward.
        //
        // Only ground worth standing on spends it. A cliff face stops the fall too, and letting it
        // count would make walking into a cliff the way to climb it: the slide turns the horizontal
        // input into motion along the surface, so with gravity cancelled every tick the player
        // strolls up a wall. Measured before this line existed: 1.9 m of height gained in two
        // seconds against a 60° face, and 0.4 m against an 80° one.
        let caught = wanted.y < 0.0 && moved.y > wanted.y + 1e-6 && self.on_ground;
        let stopped = wanted.y > 0.0 && moved.y < wanted.y - 1e-6;
        if caught || stopped {
            self.velocity.y = 0.0;
        }

        // Hold the capsule one skin above the surface while grounded, never on it and never in
        // it. A sweep leaves it a fraction of a millimetre low each tick and never puts that back,
        // which compounds into centimetres over a minute of walking, and `standing_does_not_drift_
        // downward` holds that to account. Snapping to a height the ground probe reports exactly
        // cannot accumulate at all, whatever the sweep did.
        if let Some(rest) = footing.filter(|_| self.on_ground) {
            self.position.y = rest + SKIN;
        }
    }

    /// Advances one tick of flight.
    ///
    /// **No forces at all**, which is the whole of it: the velocity *is* the intent, so there is no
    /// gravity to fall under, no acceleration to ramp through and nothing left over when the keys
    /// are let go. A flying player stops where they stop, which is what makes it possible to hold a
    /// position over a hillside and work on it.
    ///
    /// Still solid. Nothing here removes the capsule from the world — the sweep is the same one
    /// walking uses, so terrain and crates still stop a flyer. That is a deliberate choice against
    /// noclip: flight is for looking at ground from above, and ground you can pass through is
    /// ground you cannot judge the shape of.
    fn fly(&mut self, input: &PlayerInput, level: &Level, dt: f32) {
        // Never crouched in the air: the stance exists to fit under things by shrinking the
        // capsule, and a flyer goes over them instead. The key is the descent instead.
        self.crouching = false;
        self.on_ground = false;

        let local = input.local_direction();
        let (sin, cos) = input.yaw.sin_cos();
        let wish = Vec3::new(
            local.x * cos - local.y * sin,
            f32::from(input.jump) - f32::from(input.crouch),
            -local.y * cos - local.x * sin,
        );
        // Normalised whole, not per axis, so up-and-forward is the same speed as forward — the
        // same reason `local_direction` normalises the diagonals.
        self.velocity = wish.normalize_or_zero() * FLY_SPEED;

        let wanted = self.velocity * dt;
        self.position += level.sweep_capsule(self.position, wanted, self.crouching);
    }

    /// Crouching starts the moment the key is held; standing back up has to wait for headroom.
    fn update_stance(&mut self, input: &PlayerInput, level: &Level) {
        self.crouching = if input.crouch {
            true
        } else {
            !level.can_stand_up(self.position) && self.crouching
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::test_support::{ask, floor_app};
    use bevy::prelude::App;

    const DT: f32 = 1.0 / 64.0;

    /// Advances a player `ticks` times against the app's level, and hands back where it ended up.
    ///
    /// The whole run happens inside one query rather than one per tick, because a `Level` is a
    /// system parameter: getting one costs a system run, and these tests advance tens of thousands
    /// of ticks between them.
    fn run(app: &mut App, state: PlayerState, input: PlayerInput, ticks: usize) -> PlayerState {
        ask(app, move |level| {
            let mut state = state;
            for _ in 0..ticks {
                state.apply_input(&input, level, DT);
            }
            state
        })
    }

    #[test]
    fn standing_still_stays_on_the_floor() {
        let mut app = floor_app();
        let state = run(&mut app, PlayerState::default(), PlayerInput::default(), 64);
        assert!(state.on_ground);
        assert!(state.position.y.abs() < 0.05, "{:?}", state.position);
        // Gravity must not accumulate while grounded.
        assert!(state.velocity.y.abs() < 1e-3, "{}", state.velocity.y);
    }

    #[test]
    fn walking_forward_reaches_max_speed() {
        let mut app = floor_app();
        let input = PlayerInput { forward: true, ..default_input() };
        // MOVE_ACCEL_TIME is 0.3 s; a full second is comfortably enough.
        let state = run(&mut app, PlayerState::default(), input, 64);
        let speed = Vec3::new(state.velocity.x, 0.0, state.velocity.z).length();
        assert!((speed - MAX_SPEED).abs() < 0.01, "speed was {speed}");
        // Yaw 0 means forward is -Z, matching Bevy's convention.
        assert!(state.position.z < -1.0, "{:?}", state.position);
    }

    #[test]
    fn diagonals_are_not_faster() {
        let mut app = floor_app();
        let input = PlayerInput { forward: true, right: true, ..default_input() };
        let state = run(&mut app, PlayerState::default(), input, 64);
        let speed = Vec3::new(state.velocity.x, 0.0, state.velocity.z).length();
        assert!((speed - MAX_SPEED).abs() < 0.01, "diagonal speed was {speed}");
    }

    #[test]
    fn jumping_leaves_the_ground_and_lands_again() {
        let mut app = floor_app();
        let jump = PlayerInput { jump: true, ..default_input() };

        let state = run(&mut app, PlayerState::default(), jump, 1);
        assert!(!state.on_ground, "did not leave the ground");
        assert!(state.position.y > 0.0);

        // JUMP_VELOCITY 6.5 against GRAVITY -20 gives roughly 0.65 s of flight.
        let state = run(&mut app, state, default_input(), 64);
        assert!(state.on_ground, "never landed");
        assert!(state.position.y.abs() < 0.05, "{:?}", state.position);
    }

    #[test]
    fn crouching_slows_the_player_down() {
        let mut app = floor_app();
        let input = PlayerInput { forward: true, crouch: true, ..default_input() };
        let state = run(&mut app, PlayerState::default(), input, 64);
        assert!(state.crouching);
        let speed = Vec3::new(state.velocity.x, 0.0, state.velocity.z).length();
        assert!((speed - CROUCH_SPEED).abs() < 0.01, "crouch speed was {speed}");
        assert_eq!(state.eye_height(), CROUCH_EYE_HEIGHT);
    }

    /// Standing still must not sink. The capsule may settle a fraction of a tick's gravity into the
    /// floor on the first step, but that must be a one-off, not a slow descent through the level.
    #[test]
    fn standing_does_not_drift_downward() {
        let mut app = floor_app();
        let state = run(&mut app, PlayerState::default(), PlayerInput::default(), 64);
        let after_one_second = state.position.y;

        let state = run(&mut app, state, PlayerInput::default(), 64 * 60);
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
        let mut app = floor_app();
        let input = PlayerInput { forward: true, ..default_input() };

        let state = run(&mut app, PlayerState::default(), input, 64);
        let early = state.position.y;
        let state = run(&mut app, state, input, 64 * 30);
        let late = state.position.y;

        assert!((late - early).abs() < 1e-6, "sank from {early} to {late} while walking");
    }

    /// Walking has to keep working, not just start working. The drift tests above only ever checked
    /// height, which is how a total stall went unnoticed.
    #[test]
    fn walking_keeps_covering_ground() {
        let mut app = floor_app();
        let input = PlayerInput { forward: true, ..default_input() };

        let state = run(&mut app, PlayerState::default(), input, 64 * 10);
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
        let mut app = floor_app();
        let input =
            PlayerInput { forward: true, right: true, jump: true, yaw: 0.7, ..default_input() };

        let a = run(&mut app, PlayerState::default(), input, 40);
        let b = run(&mut app, PlayerState::default(), input, 40);
        assert_eq!(a, b);
    }

    fn default_input() -> PlayerInput {
        PlayerInput::default()
    }

    /// Flight is a hover, and the whole point of it: nothing falls, nothing drifts, and letting go
    /// of the keys leaves a builder exactly where they were looking from.
    ///
    /// Ten seconds, because gravity is the kind of thing that shows up as a millimetre a tick.
    #[test]
    fn flying_holds_its_height_with_nothing_pressed() {
        let mut app = floor_app();
        let start = PlayerState { position: Vec3::new(0.0, 20.0, 0.0), ..default() };
        let input = PlayerInput { flying: true, ..default_input() };
        let state = run(&mut app, start, input, 640);
        assert_eq!(state.position, start.position, "a hovering player moved");
        assert_eq!(state.velocity, Vec3::ZERO, "a hovering player kept a velocity");
        assert!(!state.on_ground, "a player twenty metres up was on the ground");
    }

    /// Up and down are the jump and crouch keys, at the same speed as everything else.
    #[test]
    fn the_jump_and_crouch_keys_are_the_lift() {
        let mut app = floor_app();
        let start = PlayerState { position: Vec3::new(0.0, 20.0, 0.0), ..default() };
        let speed = FLY_SPEED;

        let up = PlayerInput { flying: true, jump: true, ..default_input() };
        let state = run(&mut app, start, up, 64);
        assert!((state.position.y - (20.0 + speed)).abs() < 0.05, "{:?}", state.position);

        let down = PlayerInput { flying: true, crouch: true, ..default_input() };
        let state = run(&mut app, start, down, 64);
        assert!((state.position.y - (20.0 - speed)).abs() < 0.05, "{:?}", state.position);

        // And a crouch key held in the air is a descent, not a stance: the capsule stays its full
        // height, because there is nothing overhead to fit under.
        assert!(!state.crouching, "a descending player crouched");
    }

    /// One speed, whichever way it is pointed.
    ///
    /// Climbing while going forward has to cover the same ground as either alone, for the reason
    /// the diagonals are normalised: a builder should not learn to fly at 45° because it is faster.
    #[test]
    fn flight_is_one_speed_whichever_way_it_is_pointed() {
        let mut app = floor_app();
        let start = PlayerState { position: Vec3::new(0.0, 40.0, 0.0), ..default() };
        for input in [
            PlayerInput { forward: true, ..default_input() },
            PlayerInput { jump: true, ..default_input() },
            PlayerInput { forward: true, right: true, jump: true, ..default_input() },
        ] {
            let input = PlayerInput { flying: true, ..input };
            let state = run(&mut app, start, input, 64);
            let travelled = state.position.distance(start.position);
            assert!(
                (travelled - FLY_SPEED).abs() < FLY_SPEED * 0.02,
                "flight went {travelled} m in a second, not {FLY_SPEED}",
            );
        }
    }

    /// Flight is not noclip. The ground still stops a flyer, which is what makes a hillside
    /// something to land on rather than something to sink through.
    #[test]
    fn the_ground_still_stops_a_flyer() {
        let mut app = floor_app();
        let start = PlayerState { position: Vec3::new(0.0, 4.0, 0.0), ..default() };
        let down = PlayerInput { flying: true, crouch: true, ..default_input() };
        let state = run(&mut app, start, down, 64);
        assert!(state.position.y > -SKIN, "a flyer sank through the floor: {:?}", state.position);
    }

    /// Letting go of the mode gives the world back, and it gives back the fall with it.
    #[test]
    fn leaving_flight_leaves_a_player_where_gravity_can_reach_them() {
        let mut app = floor_app();
        let start = PlayerState { position: Vec3::new(0.0, 20.0, 0.0), ..default() };
        let state = run(&mut app, start, PlayerInput { flying: true, ..default_input() }, 64);
        let state = run(&mut app, state, default_input(), 64);
        assert!(state.position.y < 15.0, "a player who stopped flying stayed up: {:?}", state.position);
    }

    /// Walks forward for two seconds up a slope of `degrees` and reports where that left the
    /// player. The slope passes through the origin, which is where the player starts.
    fn walk_up(degrees: f32) -> PlayerState {
        let mut app = crate::physics::test_support::slope_app(degrees.to_radians());
        let input = PlayerInput { forward: true, ..default_input() };
        let start = PlayerState { position: Vec3::new(0.0, 0.01, 0.0), ..PlayerState::default() };
        run(&mut app, start, input, 128)
    }

    /// A hillside is walked up, and a cliff is not. That is the whole of the slope limit as a
    /// player meets it.
    ///
    /// 45° and 50° bracket [`WALKABLE_NORMAL_Y`]'s 45.6°, so this fails in one direction or the
    /// other the moment that number moves — which is the point, because it is an authoring
    /// decision and deserves to be noticed when it changes.
    ///
    /// Measured: 2.72 m of height gained in two seconds at 45°, and 22.4 m lost at 50°.
    #[test]
    fn a_hillside_is_climbed_and_a_cliff_is_slid_down() {
        let hill = walk_up(45.0);
        assert!(hill.on_ground, "standing on a 45° hillside does not count as standing");
        assert!(hill.position.y > 2.0, "only climbed {:.2} m of hillside", hill.position.y);

        let cliff = walk_up(50.0);
        assert!(!cliff.on_ground, "a 50° cliff face counts as ground to stand on");
        assert!(cliff.position.y < -1.0, "walked {:.2} m up a cliff", cliff.position.y);
    }

    /// The interesting failure is not the cliff — it is the slope just inside the limit, where the
    /// capsule's roundness lifts the feet clear of the surface. The ground probe is a ray from the
    /// feet straight down, and without [`slope_lift`](crate::movement::slope_lift) it loses the
    /// floor at about 41°: the limit would then be a consequence of the capsule's radius rather
    /// than a number anybody chose.
    #[test]
    fn the_limit_is_the_one_that_was_chosen_and_not_the_capsules_own() {
        for degrees in [41.0f32, 43.0, 45.0] {
            let state = walk_up(degrees);
            assert!(state.on_ground, "airborne on a walkable {degrees}° slope");
            assert!(
                state.position.y > 2.0,
                "at {degrees}° the climb managed {:.2} m, which is a player fighting the ground",
                state.position.y,
            );
        }
    }

    /// Gravity may not accumulate while standing, and a cliff is not standing. Both halves are
    /// here because they are the same line of code: the fall is spent against ground worth
    /// standing on and nothing else.
    #[test]
    fn a_cliff_does_not_hold_a_player_up() {
        let cliff = walk_up(60.0);
        assert!(cliff.velocity.y < -5.0, "a 60° face cancelled gravity: {}", cliff.velocity.y);
        let flat = run(&mut floor_app(), PlayerState::default(), default_input(), 128);
        assert!(flat.velocity.y.abs() < 1e-3, "gravity accumulated on the flat: {}", flat.velocity.y);
    }
}
