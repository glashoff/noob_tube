//! Automated inspection of a running client.
//!
//! Only active when `NOOB_TUBE_HARNESS` is set, so it costs nothing in a normal run. The variable
//! holds a screenshot path; the client walks a scripted route, saves the image and exits.
//!
//! This exists because "it compiles" and "it runs" say nothing about whether the player is
//! standing on the ground or falling through it. `webgame` solves the same problem with its
//! `client/scenarios/` directory.

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use lightyear::prelude::Predicted;
use noob_tube_shared::player::{PlayerInput, PlayerState};

use crate::local_player::{MovementTicks, ScriptedInput};

/// Frames to run before capturing. Enough for the ground to load and the walk to cover distance.
const CAPTURE_FRAME: u32 = 500;

/// Total frames before quitting — long enough to outlast the 3 s connection timeout.
const TOTAL_FRAMES: u32 = 900;

pub struct HarnessPlugin;

impl Plugin for HarnessPlugin {
    fn build(&self, app: &mut App) {
        let Ok(path) = std::env::var("NOOB_TUBE_HARNESS") else {
            return;
        };
        app.insert_resource(Harness {
            path: path.into(),
            frame: 0,
        })
        .add_systems(Update, drive);
    }
}

#[derive(Resource)]
struct Harness {
    path: std::path::PathBuf,
    frame: u32,
}

/// Walks forward, then captures and quits.
fn drive(
    mut harness: ResMut<Harness>,
    mut scripted: ResMut<ScriptedInput>,
    player: Option<Single<&PlayerState, With<Predicted>>>,
    virtual_time: Res<Time<Virtual>>,
    ticks: Res<MovementTicks>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    harness.frame += 1;

    // Walk forward, veering right after a while so the route grazes the crate at (0, -18)
    // instead of stopping dead against it. That exercises sliding as well as walking.
    scripted.0 = Some(PlayerInput {
        forward: true,
        right: harness.frame > 150,
        jump: harness.frame % 200 == 0,
        ..default()
    });

    // Nothing to report until the server has given us a player to predict.
    let Some(player) = player else { return };

    // Periodic trace so a stall shows up as a position that stops changing.
    if harness.frame % 60 == 0 {
        info!(
            "harness: frame {} ticks={} feet={:?} vel={:?} speed={:.3}",
            harness.frame,
            ticks.0,
            player.position,
            player.velocity,
            virtual_time.relative_speed()
        );
    }

    if harness.frame == CAPTURE_FRAME {
        let p = player.position;
        info!("harness: feet at {p:?}, on_ground={}", player.on_ground);
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(harness.path.clone()));
    }
    // Give the capture a few frames to reach disk before quitting.
    if harness.frame >= TOTAL_FRAMES {
        exit.write(AppExit::Success);
    }
}
