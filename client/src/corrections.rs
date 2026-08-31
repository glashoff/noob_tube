//! How far a rollback moves what you were already looking at.
//!
//! A rollback restores the predicted state to the last tick the server confirmed and replays
//! forward. The simulation must take that correction whole and immediately — that is the point of
//! it. What the *screen* does with it is a separate question, and right now the answer is: nothing.
//! [`place_camera`](crate::local_player) writes the corrected position on the next frame and the
//! view jumps.
//!
//! Whether that jump is worth smoothing depends on how big it is, and that is a measurement rather
//! than an opinion. Frequency is not enough — one rollback every eight seconds sounds rare, but a
//! rollback that moves the camera 25 cm is a jolt and one that moves it 2 mm is nothing.
//!
//! Three things are measured, and they answer different questions.
//!
//! **How far a rollback moves a predicted body.** Two systems inside lightyear's rollback, one
//! either side of the replay: the first captures the pose the last frame drew, the second compares
//! it with the pose the replay produced. Replayed ticks come free from [`MovementTicks`], since the
//! replay runs `FixedMain` inline between them. This is what decided that a *predicted* crate had
//! to go — a median snap of 3.7 cm and up to 79 cm, in one frame.
//!
//! **How far the drawn player jumps in one rendered frame**, over and above what moving explains. A
//! raw per-frame step is useless on its own: at 5.5 m/s and 30 fps, moving is 18 cm a frame, and a
//! 15 cm jump would hide inside it. Subtracting the distance actually covered leaves only what
//! movement cannot account for. Not gated on a rollback, because the point is the opposite:
//! measuring every frame is what makes a smoothed correction show up, as its absence.
//!
//! **How far the drawn player is from the simulation.** The price of the smoothing, and the reason
//! it cannot simply be made slower and slower: a view that never jumps but trails half a metre
//! behind the position shots are fired from is worse than the jump.
//!
//! Both are measured on the *player*, not on the camera, and that is not a detail. The camera sits
//! seven metres behind a vehicle while driving, and reading the camera's own position reported that
//! as seven metres of error. Nothing about where the camera is put belongs in a measurement of how
//! wrong the picture is.
//!
//! The player's own rollback error is deliberately *not* measured here any more. Frame
//! interpolation writes the visual pose into the live `PlayerState` in `PostUpdate` and restores
//! the simulation's in `RunFixedMainLoop`, so by the time rollback runs in `PreUpdate` the live
//! value is the one that was drawn, not the one that was simulated. Comparing it with the replay's
//! output measures the two things at once. The lag figure above answers the same question without
//! the confusion.

use avian3d::prelude::Position;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use std::collections::VecDeque;
use lightyear::prelude::{Predicted, Rollback, RollbackSystems};
use noob_tube_shared::player::PlayerState;

use crate::local_player::{MovementTicks, ViewError};

/// Everything this client simulates for itself that is not its own player: the loose crates today,
/// a vehicle it is driving later.
type PredictedBody = (With<Predicted>, Without<PlayerState>);

/// How often the running summary is logged, in seconds.
const SUMMARY_EVERY: f32 = 10.0;

/// How far back the figure on screen looks.
const WINDOW: f32 = 1.0;

pub struct CorrectionsPlugin;

impl Plugin for CorrectionsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Corrections>()
            .register_type::<Corrections>()
            .add_systems(
                PreUpdate,
                (
                    remember
                        .after(RollbackSystems::Check)
                        .before(RollbackSystems::Prepare),
                    measure
                        .after(RollbackSystems::Rollback)
                        .before(RollbackSystems::EndRollback),
                )
                    .run_if(resource_exists::<Rollback>),
            )
            .add_systems(Update, summarise)
            // `Last`, so it reads the transform the renderer will actually use — after the camera
            // has been placed and after transform propagation.
            .add_systems(Last, watch_the_camera)
            .add_systems(FixedPostUpdate, note_the_simulated_eye);
    }
}

/// What the last rollback moved, and what all of them have moved so far.
///
/// Registered for reflection so a run can be read over BRP rather than out of the log.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct Corrections {
    /// How many rollbacks have happened since the client started.
    pub count: u32,
    /// The worst any predicted body — a crate, later a vehicle — has been moved, in metres.
    pub worst_body: f32,
    /// The furthest the drawn player has ever been from where the simulation says they are.
    pub worst_lag: f32,
    /// The same over the last second, and how much of it the rollback smoothing accounted for.
    ///
    /// A second rather than a running maximum because the question a player asks is "is it doing
    /// this *now*", and an all-time worst answers that with something that happened once, on
    /// connect, a quarter of an hour ago.
    ///
    /// Both figures come from the *same frame* — the worst one — rather than being two independent
    /// maxima. Taken separately they can contradict each other outright: a share larger than the
    /// whole, from two different moments in the same second.
    pub lag_last_second: f32,
    pub smoothing_last_second: f32,
    /// The furthest the drawn player has jumped in one rendered frame beyond what moving explains.
    ///
    /// This is the number a player actually sees, and the one that says whether smoothing works: a
    /// correction changes it and not the rollback figures, because the simulation still takes the
    /// whole correction at once.
    pub worst_frame: f32,
    /// Where every predicted body was before the replay. Set by `remember`, read by `measure`.
    #[reflect(ignore)]
    bodies: HashMap<Entity, Vec3>,
    /// `MovementTicks` before the replay, so its depth can be counted.
    #[reflect(ignore)]
    ticks: u64,
    /// Seconds until the next summary, and what to say in it.
    #[reflect(ignore)]
    countdown: f32,
    #[reflect(ignore)]
    since: Vec<f32>,
    /// Where the player was drawn last frame, and the worst step since the last summary.
    #[reflect(ignore)]
    eye: Option<Vec3>,
    /// Where the simulation last put the eye, as opposed to where it was drawn, and how fast it was
    /// actually covering ground — which is how much of a frame's movement is explained.
    ///
    /// The speed is measured from the simulated position itself rather than read off `velocity`,
    /// because the two part company: a player pressed against a wall has 5.5 m/s of velocity and
    /// goes nowhere, and a driver's velocity is zero while the vehicle carries them along.
    #[reflect(ignore)]
    simulated_eye: Option<Vec3>,
    #[reflect(ignore)]
    speed: f32,
    #[reflect(ignore)]
    worst_frame_since: f32,
    #[reflect(ignore)]
    worst_lag_since: f32,
    /// The last second of samples, oldest first, as (when, total lag, smoothing's share).
    #[reflect(ignore)]
    recent: VecDeque<(f32, f32, f32)>,
}

/// PreUpdate, inside the rollback and before it snaps back: where the predicted bodies were.
fn remember(
    mut corrections: ResMut<Corrections>,
    ticks: Res<MovementTicks>,
    bodies: Query<(Entity, &Position), PredictedBody>,
) {
    corrections.ticks = ticks.0;
    corrections.bodies.clear();
    for (entity, position) in bodies.iter() {
        corrections.bodies.insert(entity, position.0);
    }
}

/// PreUpdate, after the replay and before the rollback ends: where they are instead.
fn measure(
    mut corrections: ResMut<Corrections>,
    ticks: Res<MovementTicks>,
    bodies: Query<(Entity, &Position), PredictedBody>,
) {
    let replayed = ticks.0.saturating_sub(corrections.ticks);

    // The largest any predicted body moved. One number rather than one per entity: what matters is
    // whether anything jumped visibly, not which.
    let mut worst = 0.0f32;
    for (entity, position) in bodies.iter() {
        if let Some(was) = corrections.bodies.get(&entity) {
            worst = worst.max(was.distance(position.0));
        }
    }
    corrections.worst_body = corrections.worst_body.max(worst);
    corrections.count += 1;
    corrections.since.push(worst);

    debug!("rollback: {replayed} ticks replayed, worst body {:.1} cm", worst * 100.0);
}

/// Last: how far the camera moved this frame.
///
/// Not gated on a rollback, because the point is the opposite: this measures every frame, so a
/// smoothed correction shows up as its absence. A respawn teleports and will register here as one
/// large step, which is correct — it is a jump, it is simply an intended one.
fn watch_the_camera(
    mut corrections: ResMut<Corrections>,
    time: Res<Time>,
    smoothing: Res<ViewError>,
    drawn: Option<Single<&PlayerState, With<Predicted>>>,
) {
    // Nothing to compare against until there is a player, and nothing to compare it with until the
    // simulation has run a tick: the first frame is a teleport from the origin, not a jump.
    let (Some(drawn), true) = (drawn, corrections.simulated_eye.is_some()) else {
        corrections.eye = None;
        return;
    };
    let eye = drawn.eye_position();
    if let Some(was) = corrections.eye {
        // Only the part moving cannot explain. A frame that took 30 ms legitimately moves the
        // player 18 cm at running speed, and a jump has to be told apart from that.
        let walked = corrections.speed * time.delta_secs();
        let jumped = (was.distance(eye) - walked).max(0.0);
        corrections.worst_frame = corrections.worst_frame.max(jumped);
        corrections.worst_frame_since = corrections.worst_frame_since.max(jumped);
    }
    if let Some(simulated) = corrections.simulated_eye {
        let lag = simulated.distance(eye);
        corrections.worst_lag = corrections.worst_lag.max(lag);
        corrections.worst_lag_since = corrections.worst_lag_since.max(lag);

        let now = time.elapsed_secs();
        corrections.recent.push_back((now, lag, smoothing.offset.length()));
        while corrections.recent.front().is_some_and(|(when, ..)| now - when > WINDOW) {
            corrections.recent.pop_front();
        }
        let worst = corrections
            .recent
            .iter()
            .copied()
            .max_by(|(_, a, _), (_, b, _)| a.total_cmp(b));
        if let Some((_, lag, own)) = worst {
            corrections.lag_last_second = lag;
            corrections.smoothing_last_second = own;
        }
    }
    corrections.eye = Some(eye);
}

/// FixedPostUpdate: where the simulation says the eye is, as opposed to where it is drawn.
///
/// Read here because this is the one schedule in which the live `PlayerState` is certain to hold
/// the simulated value: frame interpolation records into its history in this same schedule, and
/// only overwrites the live component later, in `PostUpdate`.
fn note_the_simulated_eye(
    mut corrections: ResMut<Corrections>,
    time: Res<Time<Fixed>>,
    player: Option<Single<&PlayerState, With<Predicted>>>,
) {
    let Some(state) = player else {
        return;
    };
    let eye = state.eye_position();
    if let Some(was) = corrections.simulated_eye {
        corrections.speed = was.distance(eye) / time.delta_secs();
    }
    corrections.simulated_eye = Some(eye);
}

/// Update: a line every ten seconds, but only when there was something to say.
///
/// A summary rather than a line per rollback, because under packet loss the per-rollback lines are
/// what a stall looks like in a log and drown everything else.
fn summarise(mut corrections: ResMut<Corrections>, time: Res<Time>) {
    corrections.countdown -= time.delta_secs();
    if corrections.countdown > 0.0 {
        return;
    }
    corrections.countdown = SUMMARY_EVERY;
    let stepped = std::mem::take(&mut corrections.worst_frame_since);
    let lag = std::mem::take(&mut corrections.worst_lag_since);
    let seen = std::mem::take(&mut corrections.since).len();
    if seen == 0 && stepped == 0.0 {
        return;
    }
    info!(
        "view: {seen} rollbacks in {SUMMARY_EVERY:.0} s, biggest jump in one frame {:.1} cm, \
         furthest the picture trailed the simulation {:.1} cm \
         (all time: worst body snap {:.1} cm)",
        stepped * 100.0,
        lag * 100.0,
        corrections.worst_body * 100.0,
    );
}
