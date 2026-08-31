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
//! This measures it. Both systems run inside lightyear's rollback, on either side of the replay:
//! the first captures the pose the last frame drew, the second compares it with the pose the
//! replay produced. The distance between them is exactly the error a correction would have to
//! decay, so it is the number that decides whether to build one.
//!
//! Replayed ticks come for free from [`MovementTicks`]: the replay runs `FixedMain` inline, so the
//! counter advances between the two systems and the difference is the depth of the rollback.

use avian3d::prelude::Position;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use lightyear::prelude::{Predicted, Rollback, RollbackSystems};
use noob_tube_shared::player::PlayerState;

use crate::local_player::MovementTicks;

/// Everything this client simulates for itself that is not its own player: the loose crates today,
/// a vehicle it is driving later.
type PredictedBody = (With<Predicted>, Without<PlayerState>);

/// How often the running summary is logged, in seconds.
const SUMMARY_EVERY: f32 = 10.0;

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
            .add_systems(Update, summarise);
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
    /// The worst the camera has ever been moved by one, in metres.
    pub worst: f32,
    /// Every metre the camera has been moved, added up. Divided by `count`, the mean.
    pub total: f32,
    /// The worst any predicted body — a crate, later a vehicle — has been moved, in metres.
    pub worst_body: f32,
    /// The camera's position before the replay. Set by `remember`, read by `measure`.
    #[reflect(ignore)]
    before: Option<Vec3>,
    /// The same for every predicted body.
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
}

/// PreUpdate, inside the rollback and before it snaps back: what the last frame drew.
fn remember(
    mut corrections: ResMut<Corrections>,
    ticks: Res<MovementTicks>,
    player: Option<Single<&PlayerState, With<Predicted>>>,
    bodies: Query<(Entity, &Position), PredictedBody>,
) {
    corrections.before = player.map(|state| state.position);
    corrections.ticks = ticks.0;
    corrections.bodies.clear();
    for (entity, position) in bodies.iter() {
        corrections.bodies.insert(entity, position.0);
    }
}

/// PreUpdate, after the replay and before the rollback ends: what it will draw instead.
fn measure(
    mut corrections: ResMut<Corrections>,
    ticks: Res<MovementTicks>,
    player: Option<Single<&PlayerState, With<Predicted>>>,
    bodies: Query<(Entity, &Position), PredictedBody>,
) {
    let replayed = ticks.0.saturating_sub(corrections.ticks);

    // The largest any predicted body moved. One number rather than one per crate: what matters is
    // whether anything jumped visibly, not which.
    let mut body = 0.0f32;
    for (entity, position) in bodies.iter() {
        if let Some(was) = corrections.bodies.get(&entity) {
            body = body.max(was.distance(position.0));
        }
    }
    corrections.worst_body = corrections.worst_body.max(body);

    let (Some(before), Some(after)) = (corrections.before, player.map(|state| state.position))
    else {
        return;
    };
    let moved = before.distance(after);
    corrections.count += 1;
    corrections.total += moved;
    corrections.worst = corrections.worst.max(moved);
    corrections.since.push(moved);

    debug!(
        "rollback: {replayed} ticks replayed, camera moved {:.1} cm, worst body {:.1} cm",
        moved * 100.0,
        body * 100.0
    );
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
    if corrections.since.is_empty() {
        return;
    }

    let since = std::mem::take(&mut corrections.since);
    let worst = since.iter().copied().fold(0.0f32, f32::max);
    let mean = since.iter().sum::<f32>() / since.len() as f32;
    info!(
        "corrections: {} in {SUMMARY_EVERY:.0} s, mean {:.1} cm, worst {:.1} cm \
         (all time: {} rollbacks, worst camera {:.1} cm, worst body {:.1} cm)",
        since.len(),
        mean * 100.0,
        worst * 100.0,
        corrections.count,
        corrections.worst * 100.0,
        corrections.worst_body * 100.0,
    );
}
