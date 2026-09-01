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

use crate::physics::Level;
use crate::player::{Aim, PlayerInput, PlayerState};
use crate::vehicle::Driving;

/// A player on their feet: what they are asking for and where they are.
type Walking = (&'static ActionState<PlayerInput>, &'static mut PlayerState);

/// FixedUpdate: advances every matching player by exactly one tick.
///
/// Except the ones in a vehicle. A seated player has no legs: their pose comes from the vehicle,
/// and running the walking step on them as well would have two things writing the same position
/// every tick.
///
/// During a rollback lightyear re-runs the whole `FixedMain` schedule once per replayed tick, so
/// this is also the replay. It must therefore read nothing but its arguments: `Time<Fixed>` gives
/// the constant tick length rather than the frame time, and the input comes from `ActionState`,
/// which the input plugin refills from its buffer while replaying.
pub fn step_players<F: QueryFilter + 'static>(
    level: Level,
    time: Res<Time<Fixed>>,
    mut players: Query<Walking, (F, Without<Driving>)>,
) {
    let dt = time.delta_secs();

    for (action, mut state) in players.iter_mut() {
        state.apply_input(&action.0, &level, dt);
    }
}

/// FixedUpdate: turns every player's head, seated or not.
///
/// Separate from [`step_players`] and deliberately *not* filtered by [`Driving`], which is the
/// whole point of it being its own system. Getting into a vehicle takes away your legs; it does not
/// take away your head. A driver still looks around, still aims, still shoots from the seat — and
/// while this lived inside the walking step, their aim froze at the angle they got in at. On the
/// server that froze the angle everyone else was told about, so a driver's head and the gun on
/// their bonnet both stared at whatever they had last been looking at on foot.
///
/// Aim is a pure function of the input, which is what makes it safe to predict: replaying a tick
/// reproduces the same angles from the same buffered input, so a rollback cannot make another
/// player's head twitch.
pub fn look_around<F: QueryFilter + 'static>(
    mut players: Query<(&ActionState<PlayerInput>, &mut Aim), F>,
) {
    for (action, mut aim) in players.iter_mut() {
        aim.yaw = action.0.yaw;
        aim.pitch = action.0.pitch;
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::test_support::floor_app;
    use bevy::ecs::system::RunSystemOnce;

    /// Where a player is looking, and a held-down forward key.
    fn looking(yaw: f32, pitch: f32) -> ActionState<PlayerInput> {
        ActionState(PlayerInput { forward: true, yaw, pitch, ..PlayerInput::default() })
    }

    /// Getting into a vehicle takes away your legs, not your head.
    ///
    /// While this lived inside the walking step, which skips a seated player, a driver's aim froze
    /// at the angle they got in at — and everything downstream believed it, because everything
    /// downstream is told this number rather than the input behind it: their head on every other
    /// screen, and the gun on the beam, which held one world direction for the whole round and
    /// corrected for the chassis turning underneath it while it did.
    #[test]
    fn a_driver_still_turns_their_head() {
        let mut app = floor_app();
        let seated = app
            .world_mut()
            .spawn((looking(1.25, -0.4), Aim::default(), PlayerState::default(), Driving))
            .id();
        let afoot = app
            .world_mut()
            .spawn((looking(1.25, -0.4), Aim::default(), PlayerState::default()))
            .id();

        app.world_mut().run_system_once(look_around::<()>).expect("aim");

        for who in [seated, afoot] {
            let aim = *app.world().entity(who).get::<Aim>().expect("an aim");
            assert!(
                (aim.yaw - 1.25).abs() < 1e-6 && (aim.pitch + 0.4).abs() < 1e-6,
                "looking at 1.25/-0.4, aim came out {aim:?}"
            );
        }
    }

    /// And the legs really are taken away: the seat moves a driver, not their own feet, or two
    /// things write the same position every tick.
    #[test]
    fn a_driver_does_not_walk() {
        let mut app = floor_app();
        let seated = app
            .world_mut()
            .spawn((looking(0.0, 0.0), Aim::default(), PlayerState::default(), Driving))
            .id();
        let afoot = app
            .world_mut()
            .spawn((looking(0.0, 0.0), Aim::default(), PlayerState::default()))
            .id();

        app.world_mut().run_system_once(step_players::<()>).expect("step");

        let moved = |who: Entity| {
            app.world().entity(who).get::<PlayerState>().expect("a state").position
                != PlayerState::default().position
        };
        assert!(!moved(seated), "a driver walked out of their own seat");
        assert!(moved(afoot), "the walking step did not move anyone at all");
    }
}
