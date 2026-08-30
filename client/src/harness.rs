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
use noob_tube_shared::player::PlayerInput;

use crate::local_player::{LocalPlayer, ScriptedInput};

/// Frames to run before capturing. Enough for the ground to load and the walk to cover distance.
const CAPTURE_FRAME: u32 = 180;

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
    player: Single<&LocalPlayer>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    harness.frame += 1;

    // Walk toward the crates, which sit along -Z.
    scripted.0 = Some(PlayerInput {
        forward: true,
        ..default()
    });

    if harness.frame == CAPTURE_FRAME {
        let p = player.state.position;
        info!("harness: feet at {p:?}, on_ground={}", player.state.on_ground);
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(harness.path.clone()));
    }
    // Give the capture a few frames to reach disk before quitting.
    if harness.frame == CAPTURE_FRAME + 30 {
        exit.write(AppExit::Success);
    }
}
