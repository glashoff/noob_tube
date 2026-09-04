//! A client that plays by itself: walks to a vehicle, gets in, and drives back and forth.
//!
//! It exists because half the questions about this project need **two** clients and one keyboard
//! cannot answer them. Two predicted vehicles meeting each other is the case with no measurement
//! anywhere in the README, and every attempt to stand a second player in — driving a parked vehicle
//! from outside, stripping a replication target off one — measured something adjacent instead of
//! the thing asked about. A bot is a real second peer with a real connection, a real prediction
//! window and a real set of inputs, which is the only kind of second player worth measuring
//! against.
//!
//! It is not an AI and is not trying to be. It is a fixed errand run in a loop, written so that a
//! human watching it can tell at a glance which step it is on and why it is stuck if it is.
//!
//! ### How it drives
//!
//! Through [`ScriptedInput`], which is the same door the test harness uses — so a bot goes through
//! prediction, rollback, input redundancy and the link conditioner exactly as a person does. What it
//! must **not** do is write `PlayerInput::yaw` directly: `sample_input` overwrites that from
//! [`LocalPlayer`] whether the input is scripted or not, because the look angles are the one thing a
//! client is authoritative over. So the bot turns by setting the same field a mouse would.

use avian3d::prelude::{LinearVelocity, Position, Rotation};
use bevy::prelude::*;
use lightyear::prelude::Predicted;
use noob_tube_shared::physics::Level;
use noob_tube_shared::player::{Player, PlayerInput, PlayerState};
use noob_tube_shared::vehicle::{Driven, Driving, VehicleKind};

use crate::local_player::{LocalPlayer, ScriptedInput};

/// How close to a vehicle the bot walks before reaching for the door.
///
/// The server's own reach is four metres, measured to the chassis centre. Three leaves room for the
/// half tick of travel between deciding and the server agreeing, and for the bot arriving at a
/// corner of the bodywork rather than at the middle of it.
const CLOSE_ENOUGH: f32 = 3.0;

/// How far the bot looks for a clear run before settling on one, in metres.
///
/// It picks its beat by casting a ray along each of the four directions the vehicle could set off
/// in and taking the longest. Without that it drives along whatever heading the vehicle happened to
/// be parked on — which in this level points the first buggy straight at the ramp, and a bot that
/// wedges itself under the lip of a ramp stops being a second player. Sensing rather than knowing:
/// nothing here is told where the ramp is.
const LOOK_AHEAD: f32 = 80.0;

/// How far out the patrol goes, in metres.
///
/// A fixed distance rather than a fixed time, and that is the whole difference between a patrol and
/// a bot walking across the map. Reverse is slower than forward, so "out for four seconds, back for
/// four seconds" drifts a few metres every cycle; measured, it wandered 470 m in half a minute and
/// went over the edge of the level, where it read as a vehicle doing 63 m/s because it was falling.
/// Sixty metres is a long enough run to pass 20 m/s and short enough to stay on the plane.
const PATROL_METRES: f32 = 60.0;

/// How close to either end the bot has to get before it counts as arrived.
const CLOSE_ENOUGH_TO_TURN: f32 = 5.0;

/// The longest the bot will spend on one leg before turning round regardless.
///
/// A leg can fail outright — a crate under a wheel, a wall behind it — and a bot waiting forever to
/// reach a point it cannot reach has quietly stopped being a second player. Generous, because
/// reverse is slow, not because it is expected to be needed.
const PATIENCE: f32 = 15.0;

/// Below this speed, while asking to move, the bot counts as stuck rather than slow.
const STALLED_BELOW: f32 = 0.5;
/// How long it tolerates that before trying to free itself.
const STALLED_SECONDS: f32 = 2.0;
/// How long a escape attempt lasts: the other direction, with the wheels on full lock.
///
/// Reversing alone does not free a wedge — measured, the bot spent forty seconds under the lip of
/// the ramp with its suspension fully compressed, and turning round on the clock did not help
/// because the way out was not the way in. Full lock is what turns a reverse into a way out.
const ESCAPE_SECONDS: f32 = 2.5;

/// The furthest the bot may ever get from where it first sat down, in metres.
///
/// A leash, and it is here because the patrol logic has run away twice — once by reversing with the
/// steering sign inverted, once by re-anchoring its beat after every escape until the anchor had
/// walked a kilometre. Both times it read as a vehicle doing several hundred metres a second,
/// because by then it was off the edge of the plane and falling. Whatever else is wrong, this
/// cannot be: past this distance the only target is home.
const LEASH: f32 = 120.0;

/// How far off the line the nose has to be before the bot corrects it, as the sine of the angle.
///
/// A tenth is about six degrees. Without a dead band the bot saws at the wheel every tick, which
/// scrubs speed off through the tyre model and looks like a fault in the vehicle rather than in the
/// driver.
const STEERING_DEADBAND: f32 = 0.1;

/// Seconds to hold the interact key, and to wait afterwards.
///
/// Getting in is not predicted, so the answer takes a round trip to arrive. Holding it briefly and
/// then waiting is what makes the bot press the key *once* rather than sixty-four times, which
/// would climb in and straight back out again.
const REACH_SECONDS: f32 = 0.2;
const SETTLE_SECONDS: f32 = 1.0;

pub struct BotPlugin;

impl Plugin for BotPlugin {
    fn build(&self, app: &mut App) {
        if !wanted() {
            return;
        }
        info!("this client is a bot");
        app.register_type::<Errand>()
            .init_resource::<Errand>()
            .add_systems(Update, run_the_errand);
    }
}

/// Whether this client is a bot, from the environment.
///
/// An environment variable rather than a config field, and for the same reason
/// `NOOB_TUBE_HEADLESS` is one: it describes *this process*, not the session. Two clients reading
/// one config file must not both become bots because one of them was meant to.
///
/// Checked inside `build` rather than at the call site, so that a client that is not a bot carries
/// none of this — no resource, no system, and above all nothing writing [`ScriptedInput`], which
/// would take the keyboard away from whoever is playing.
fn wanted() -> bool {
    crate::platform::switched_on("NOOB_TUBE_BOT")
}

/// This client's own player: where it is, who it is, and whether it is in a seat.
type Own = (
    &'static PlayerState,
    &'static Player,
    Has<Driving>,
);

/// What the bot is doing, and how long it has been doing it.
///
/// A resource rather than a component, because there is exactly one of these per process — a bot is
/// a whole client, not an entity in somebody's world. Registered for reflection so that a stuck bot
/// can be interrogated over BRP rather than guessed at from its position.
#[derive(Resource, Reflect, Debug, Default)]
#[reflect(Resource)]
enum Errand {
    /// Walking toward the nearest vehicle nobody is in.
    #[default]
    Walking,
    /// Standing at the door with the key held, then waiting for the server to answer.
    Boarding { seconds: f32 },
    /// Driving a fixed beat between two points, forever, always forwards.
    ///
    /// Both ends are worked out once, when the bot gets in, from where the vehicle was and which
    /// way it was pointing. Storing them is what makes the patrol a *place* rather than a
    /// direction: the bot steers toward whichever end it is heading for, so it comes back even
    /// after a crate has knocked it sideways.
    ///
    /// Forwards both ways, with a U-turn at each end, and that is a correction rather than a
    /// choice. Reversing back down the outward line needs the steering sign inverted, and getting
    /// that wrong does not look like a wrong sign — it looks like a bot calmly driving off the edge
    /// of the level, which is what it did: 500 m out and still accelerating, because by then it was
    /// falling. One direction has one convention and cannot be got backwards.
    Driving {
        outbound: bool,
        seconds: f32,
        home: Vec3,
        away: Vec3,
        /// Seconds spent going nowhere while asking to move.
        stalled: f32,
        /// Seconds left of an escape attempt, or zero when not making one.
        escaping: f32,
    },
}

/// Update: does the next thing.
///
/// One system rather than one per step, because the steps share everything they look at and the
/// whole of the logic fits in a screen. Splitting it would buy separate run conditions and cost the
/// property that matters here: that a person can read what the bot will do next in one place.
fn run_the_errand(
    time: Res<Time>,
    mut errand: ResMut<Errand>,
    mut scripted: ResMut<ScriptedInput>,
    vehicles: Query<(&Position, &Rotation, &LinearVelocity, Option<&Driven>), With<VehicleKind>>,
    me: Option<Single<Own, With<Predicted>>>,
    view: Option<Single<&mut LocalPlayer>>,
    level: Level,
) {
    let (Some(me), Some(mut view)) = (me, view) else {
        // Not connected yet, or the server has not sent us a body. Stand still rather than walking
        // into whatever the default input happens to be.
        scripted.0 = Some(PlayerInput::default());
        return;
    };
    let (state, who, driving) = *me;
    let dt = time.delta_secs();
    let mut input = PlayerInput::default();
    // Where this bot's own vehicle is and which way it points, if it is in one. Found by peer
    // rather than by prediction: a bot predicts every vehicle it can reach, and only one of them
    // is the one it is sitting in.
    let mine = driving
        .then(|| {
            vehicles.iter().find_map(|(at, facing, speed, driven)| {
                (driven.is_some_and(|driven| driven.0 == who.peer)).then(|| {
                    (
                        at.0,
                        (facing.0 * Vec3::NEG_Z).normalize_or_zero(),
                        speed.0.length(),
                    )
                })
            })
        })
        .flatten();

    match &mut *errand {
        Errand::Walking => {
            // The nearest one with nobody in it. A bot that walked to an occupied vehicle would
            // stand at the door pressing a key that can never work.
            let target = vehicles
                .iter()
                .filter(|(.., taken)| taken.is_none())
                .map(|(at, ..)| at.0)
                .min_by(|a, b| {
                    a.distance(state.position)
                        .total_cmp(&b.distance(state.position))
                });
            let Some(target) = target else {
                scripted.0 = Some(input);
                return;
            };
            let toward = target - state.position;
            // Yaw is measured so that zero looks down −Z, which is forward everywhere in this game.
            view.yaw = (-toward.x).atan2(-toward.z);
            if toward.length() <= CLOSE_ENOUGH {
                *errand = Errand::Boarding { seconds: 0.0 };
            } else {
                input.forward = true;
            }
        }
        Errand::Boarding { seconds } => {
            *seconds += dt;
            input.interact = *seconds < REACH_SECONDS;
            if let Some((at, facing, _)) = mine {
                *errand = Errand::Driving {
                    outbound: true,
                    seconds: 0.0,
                    home: at,
                    away: at + clearest(&level, at, facing),
                    stalled: 0.0,
                    escaping: 0.0,
                };
            } else if *seconds > REACH_SECONDS + SETTLE_SECONDS {
                // The door did not open — somebody beat us to it, or we stopped short. Walk again
                // rather than standing there pressing a key for the rest of the round.
                *errand = Errand::Walking;
            }
        }
        Errand::Driving {
            outbound,
            seconds,
            home,
            away,
            stalled,
            escaping,
        } => {
            let Some((at, facing, speed)) = mine else {
                // Out of the seat, or the vehicle has not arrived on this client yet.
                *errand = Errand::Walking;
                scripted.0 = Some(input);
                return;
            };

            if *escaping > 0.0 {
                // Backwards, on full lock. Which way it turns does not matter and is not chosen:
                // what frees a wedge is that the car leaves along a different line from the one it
                // arrived on. This is the only place the bot ever selects reverse.
                *escaping -= dt;
                input.backward = true;
                input.left = true;
                if *escaping <= 0.0 {
                    // Head for the other end of the same beat. Re-anchoring here was tried and is
                    // what let the patrol walk across the map: an anchor that moves every time the
                    // bot gets stuck is not an anchor.
                    *outbound = !*outbound;
                    *seconds = 0.0;
                }
                scripted.0 = Some(input);
                return;
            }

            *seconds += dt;
            *stalled = if speed < STALLED_BELOW { *stalled + dt } else { 0.0 };
            if *stalled >= STALLED_SECONDS {
                *stalled = 0.0;
                *escaping = ESCAPE_SECONDS;
                scripted.0 = Some(input);
                return;
            }

            if at.distance(*home) > LEASH {
                // Whatever it thought it was doing, it is doing it too far away. See [`LEASH`].
                *outbound = false;
                *seconds = 0.0;
            }
            let target = if *outbound { *away } else { *home };
            if at.distance(target) <= CLOSE_ENOUGH_TO_TURN || *seconds >= PATIENCE {
                *outbound = !*outbound;
                *seconds = 0.0;
            }
            input.forward = true;

            // Which way the nose has to swing to point at the end being driven to: the sine of the
            // angle, from the cross product of two flattened unit vectors. Negative means the
            // target is off to the right, because rotating −Z about +Y by a positive angle goes
            // left and the wheels use the opposite sign convention.
            let toward = (target - at) * Vec3::new(1.0, 0.0, 1.0);
            let side = facing.cross(toward.normalize_or_zero()).y;
            if side.abs() > STEERING_DEADBAND {
                input.right = side < 0.0;
                input.left = !input.right;
            }
        }
    }
    scripted.0 = Some(input);
}

/// Which way to lay out the patrol, and how long to make it.
///
/// Four rays, along the vehicle's own axes, and the longest clear one wins. Each starts a car's
/// length out so it does not simply hit the chassis it came from — the footing filter sees bodies as
/// well as the map, which is what makes it notice a parked vehicle in the way and is also what makes
/// it notice this one.
///
/// The answer is a displacement rather than a direction, because how far is part of the question: a
/// beat that ends inside the thing the ray found is not a beat.
fn clearest(level: &Level, at: Vec3, facing: Vec3) -> Vec3 {
    const NOSE: f32 = 3.0;
    let right = facing.cross(Vec3::Y).normalize_or_zero();
    let room = |direction: Vec3| {
        level
            .surface_hit(at + direction * NOSE + Vec3::Y, direction, LOOK_AHEAD)
            .map(|(distance, ..)| distance)
            .unwrap_or(LOOK_AHEAD)
    };
    [facing, -facing, right, -right]
        .into_iter()
        .map(|direction| (direction, room(direction)))
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        // Stop short of whatever the ray found, and never make the beat so short that the vehicle
        // spends all of it turning round.
        .map(|(direction, room)| direction * room.clamp(15.0, PATROL_METRES))
        .unwrap_or(facing * PATROL_METRES)
}
