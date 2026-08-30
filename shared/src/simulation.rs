//! The one movement step, run by the server and replayed by every client's prediction.
//!
//! There is deliberately a single system here rather than one per binary. Prediction only works if
//! replaying a tick locally lands exactly where the server puts it, and two copies of the same six
//! lines is precisely how that stops being true — one of them gains a condition, and the drift it
//! causes shows up as a correction the player feels, not as a compile error.
//!
//! The query filter is the only thing that differs: the server steps every player, a client steps
//! only the entity lightyear marked [`Predicted`](lightyear::prelude::Predicted).

use bevy::ecs::query::QueryFilter;
use bevy::prelude::*;
use lightyear::prelude::input::native::ActionState;

use crate::collision::CollisionWorld;
use crate::player::{Aim, PlayerInput, PlayerState};

/// FixedUpdate: advances every matching player by exactly one tick.
///
/// During a rollback lightyear re-runs the whole `FixedMain` schedule once per replayed tick, so
/// this is also the replay. It must therefore read nothing but its arguments: `Time<Fixed>` gives
/// the constant tick length rather than the frame time, and the input comes from `ActionState`,
/// which the input plugin refills from its buffer while replaying.
pub fn step_players<F: QueryFilter + 'static>(
    world: Option<Res<CollisionWorld>>,
    time: Res<Time<Fixed>>,
    mut players: Query<(&ActionState<PlayerInput>, &mut PlayerState, &mut Aim), F>,
) {
    // The collision world is built in Startup, which can land after the first fixed tick.
    let Some(world) = world else { return };
    let dt = time.delta_secs();

    for (action, mut state, mut aim) in players.iter_mut() {
        let input = action.0;
        state.apply_input(&input, &world, dt);
        // Aim is a pure function of the input, which is what makes it safe to predict: replaying a
        // tick reproduces the same angles from the same buffered input, so a rollback cannot make
        // another player's head twitch.
        aim.yaw = input.yaw;
        aim.pitch = input.pitch;
    }
}
