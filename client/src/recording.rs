//! Writing down what a player did, so that somebody else can look at it afterwards.
//!
//! **F9** starts and stops it; `NOOB_TUBE_RECORD=<path>` starts it at launch. What comes out is one
//! JSON object per line — a header, then one line per fixed tick — which means `grep`, `jq` and a
//! text editor all work on it and nothing has to be written to read it.
//!
//! What a line holds is the *input* and the *result*: what was asked for that tick, and where the
//! player and the vehicle ended up. Both halves are needed and for different reasons. The input is
//! what can be replayed; the result is what says whether the replay reproduced anything. A trace of
//! inputs alone cannot tell "it happened again" from "it did not".
//!
//! **`NOOB_TUBE_REPLAY=<path>` plays one back**, feeding each tick's input through
//! [`ScriptedInput`](crate::local_player::ScriptedInput) — the same door the harness and the bot go
//! through, which is the whole reason that door exists. It also compares where the player actually
//! ended up against where the recording says they did, and says so when the two part company.
//!
//! It will **not** reproduce a session exactly, and it is worth being plain about why rather than
//! discovering it: the server is a different process with its own state, the map may differ, other
//! players are not in the file, and the network is not the network it was. What a replay is for is
//! putting the same gesture into a live world and watching. What the *trace* is for is reading what
//! happened, and that part is exact.

use bevy::prelude::*;
use lightyear::prelude::{LocalTimeline, Predicted};
use noob_tube_shared::player::{PlayerInput, PlayerState};
use noob_tube_shared::vehicle::Driving;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use crate::local_player::{CurrentInput, ScriptedInput};

/// Where a recording goes when nobody said.
const DEFAULT_PATH: &str = "noob-tube-recording.jsonl";

pub struct RecordingPlugin;

impl Plugin for RecordingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Recorder>()
            .register_type::<Recorder>()
            .add_systems(Startup, start_if_asked)
            .add_systems(Update, toggle)
            // In the fixed schedule and after the movement step, so a line is one tick and the
            // pose on it is what that tick produced rather than what it started from.
            .add_systems(
                FixedUpdate,
                write_a_tick.after(noob_tube_shared::simulation::step_players::<With<Predicted>>),
            );

        let Ok(path) = std::env::var("NOOB_TUBE_REPLAY") else {
            return;
        };
        app.insert_resource(Replay::read(path.into()))
            // Before `sample_input` reads it, which happens in `Update`; `FixedMain` runs first
            // inside a frame, so a value written here is this frame's input.
            .add_systems(FixedUpdate, drive_the_replay);
    }
}

/// The open file, if one is open.
///
/// Registered for reflection so a session can be asked whether it is recording without looking at
/// the screen — the same reason the menu and the brush are.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct Recorder {
    /// Whether a file is open. The file itself is not reflectable and does not need to be.
    pub running: bool,
    pub path: String,
    pub ticks: u64,
    #[reflect(ignore)]
    file: Option<BufWriter<File>>,
    /// Things worth attaching to the tick being written, put here by whoever did them.
    #[reflect(ignore)]
    notes: Vec<String>,
}

impl Recorder {
    /// Notes something that happened this tick — a stroke sent, a map loaded, a mode toggled.
    ///
    /// The point of it: an input trace says which keys were down, and almost every interesting
    /// moment in this game is something that happened *between* keys and ground. A note is how
    /// that reaches the file without a second log to line up against this one by timestamp.
    pub fn note(&mut self, what: impl Into<String>) {
        if self.running {
            self.notes.push(what.into());
        }
    }

    fn start(&mut self, path: PathBuf) {
        self.stop();
        match File::create(&path) {
            Ok(file) => {
                self.file = Some(BufWriter::new(file));
                self.running = true;
                self.ticks = 0;
                self.path = path.display().to_string();
                info!("recording to {}", self.path);
            }
            Err(error) => error!("cannot record to {}: {error}", path.display()),
        }
    }

    fn stop(&mut self) {
        if let Some(mut file) = self.file.take() {
            // Flushed by hand rather than left to the drop, because a recording is usually wanted
            // after something went wrong, and a crash is exactly when a buffered tail is lost.
            if let Err(error) = file.flush() {
                error!("the recording's tail did not reach the disk: {error}");
            }
            info!("recorded {} ticks to {}", self.ticks, self.path);
        }
        self.running = false;
    }

    fn write(&mut self, line: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };
        if let Err(error) = writeln!(file, "{line}") {
            error!("the recording stopped: {error}");
            self.file = None;
            self.running = false;
            return;
        }
        self.ticks += 1;
        // Every second, so a session killed with the window still holds all but the last second.
        if self.ticks.is_multiple_of(64) {
            let _ = file.flush();
        }
    }
}

/// Startup: begins a recording if the environment asked for one.
fn start_if_asked(mut recorder: ResMut<Recorder>) {
    if let Ok(path) = std::env::var("NOOB_TUBE_RECORD") {
        let path = if path.is_empty() { DEFAULT_PATH.to_string() } else { path };
        recorder.start(path.into());
    }
}

/// Update: F9 starts one, F9 stops it.
///
/// A function key for the reason every command in the menu is one: the letters belong to whatever
/// is being typed. F9 rather than a menu entry because the moment worth recording is usually the
/// one already happening, and going through three dialogs to reach it is how it gets missed.
fn toggle(keys: Res<ButtonInput<KeyCode>>, mut recorder: ResMut<Recorder>) {
    if !keys.just_pressed(KeyCode::F9) {
        return;
    }
    if recorder.running {
        recorder.stop();
    } else {
        let path = std::env::var("NOOB_TUBE_RECORD").unwrap_or_else(|_| DEFAULT_PATH.to_string());
        recorder.start(path.into());
    }
}

/// What the player was in the middle of, which is three things from three places.
#[derive(bevy::ecs::system::SystemParam)]
struct Doing<'w, 's> {
    input: Res<'w, CurrentInput>,
    chisel: Res<'w, crate::sculpting::Chisel>,
    local: Single<'w, 's, &'static crate::local_player::LocalPlayer>,
    player: Option<Single<'w, 's, &'static PlayerState, With<Predicted>>>,
    driving: Option<TheSeat<'w, 's>>,
}

/// The vehicle this client is driving, if it is driving one.
type TheSeat<'w, 's> = Single<'w, 's, Entity, (With<Driving>, With<Predicted>)>;

/// FixedUpdate: one line, one tick.
fn write_a_tick(
    timeline: Res<LocalTimeline>,
    doing: Doing,
    mut recorder: ResMut<Recorder>,
    mut last: Local<String>,
) {
    let Doing { input, chisel, local, player, driving } = doing;
    if !recorder.running {
        return;
    }
    let notes = std::mem::take(&mut recorder.notes);
    let mode = describe_mode(&local, &chisel);
    let changed = (mode != *last).then(|| {
        last.clone_from(&mode);
        mode
    });
    let state = player.map(|player| *player.into_inner());
    let line = Tick {
        t: timeline.tick().0,
        i: input.0,
        p: state.map(|state| state.position.to_array()),
        v: state.map(|state| state.velocity.to_array()),
        g: state.map(|state| state.on_ground),
        seat: driving.map(|driving| driving.into_inner().to_bits()),
        m: changed,
        n: notes,
    };
    match serde_json::to_string(&line) {
        Ok(text) => recorder.write(&text),
        Err(error) => error!("a tick could not be written down: {error}"),
    }
}

/// What the player is in the middle of, in as few characters as will say it.
fn describe_mode(
    local: &crate::local_player::LocalPlayer,
    chisel: &crate::sculpting::Chisel,
) -> String {
    let mut out = String::new();
    if local.flying {
        out.push_str("flying");
    }
    if chisel.on {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&crate::sculpting::readout(chisel));
    }
    if out.is_empty() {
        out.push_str("walking");
    }
    out
}

/// One tick, as it appears in the file.
///
/// Short field names on purpose. This is sixty-four lines a second and the names are repeated on
/// every one of them; `p` costs three bytes where `position` costs ten, and there is a key at the
/// top of this file for anybody reading.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct Tick {
    /// The tick this line is.
    t: u32,
    /// What was asked for.
    i: PlayerInput,
    /// Where the player ended up, and how fast, and whether they were standing on anything.
    p: Option<[f32; 3]>,
    v: Option<[f32; 3]>,
    g: Option<bool>,
    /// The vehicle being driven, by entity bits, or absent for somebody on foot.
    seat: Option<u64>,
    /// What mode the player was in, written **only on the ticks it changed**.
    ///
    /// A mode is a thing you are in for thousands of ticks, so writing it on every line would be
    /// the same string sixty-four times a second. On change only, and the reader carries it
    /// forward — which is also how it reads: a line with an `m` is the moment something was
    /// switched, and those are the lines worth finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m: Option<String>,
    /// Anything that happened this tick which the keys do not say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    n: Vec<String>,
}

/// A recording being played back.
#[derive(Resource)]
struct Replay {
    ticks: Vec<Tick>,
    at: usize,
    /// Where each run was when the replay started, so what is compared is what the player *did*
    /// rather than where they happen to have been put.
    ///
    /// Without this the first number out of a replay is the distance between two spawn points —
    /// two metres, on this server, because spawns are handed out by arrival order and the replay
    /// client is not the same arrival. That is a true statement about nothing.
    from_recorded: Option<Vec3>,
    from_live: Option<Vec3>,
    /// The worst the two paths have been apart, in metres.
    worst: f32,
    /// The last tick a divergence was reported at, so a growing gap is not sixty-four lines a
    /// second of the same news.
    said_at: usize,
}

impl Replay {
    fn read(path: PathBuf) -> Self {
        let ticks = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .lines()
                .filter_map(|line| serde_json::from_str::<Tick>(line).ok())
                .collect::<Vec<_>>(),
            Err(error) => {
                error!("cannot read the recording {}: {error}", path.display());
                Vec::new()
            }
        };
        info!("replaying {} ticks from {}", ticks.len(), path.display());
        Self { ticks, at: 0, from_recorded: None, from_live: None, worst: 0.0, said_at: 0 }
    }
}

/// FixedUpdate: puts the next recorded tick's input where the game reads its input from.
///
/// By position in the file rather than by tick number: the recorded ticks belong to another
/// session's clock, and lining them up against this one's would put the whole replay wherever the
/// two happened to start. What is being replayed is a *sequence of intents*, and the first one goes
/// on the first tick after the file is opened.
fn drive_the_replay(
    mut replay: ResMut<Replay>,
    player: Option<Single<&PlayerState, With<Predicted>>>,
    mut local: Single<&mut crate::local_player::LocalPlayer>,
    mut scripted: ResMut<ScriptedInput>,
) {
    // Nothing until there is a player to drive, so a replay does not spend its first second on a
    // client that has not been given one yet.
    let Some(player) = player else {
        return;
    };
    let Some(tick) = replay.ticks.get(replay.at).cloned() else {
        if scripted.0.is_some() {
            scripted.0 = None;
            info!("the replay is over, worst divergence {:.2} m", replay.worst);
        }
        return;
    };
    replay.at += 1;
    scripted.0 = Some(tick.i);
    // The look angles too, and they are not optional. `sample_input` takes yaw and pitch from the
    // player rather than from the script — deliberately, so a script can steer by writing them and
    // leave the rest of the input alone — so a replay that only set the keys would walk the whole
    // recording in whatever direction this client happened to be facing. Measured before this
    // line: 38 m adrift inside twenty seconds, with every key correct.
    local.yaw = tick.i.yaw;
    local.pitch = tick.i.pitch;

    // What makes this a measurement rather than a puppet show. A replay cannot reproduce a session
    // exactly — different server, different map state, nobody else in the file — so the useful
    // question is not "did it match" but "where did it stop matching".
    let Some(recorded) = tick.p.map(Vec3::from_array) else {
        return;
    };
    let live = player.position;
    let (start_recorded, start_live) = match (replay.from_recorded, replay.from_live) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            replay.from_recorded = Some(recorded);
            replay.from_live = Some(live);
            (recorded, live)
        }
    };
    // Displacement against displacement. The two runs start in different places and that is not a
    // divergence; going somewhere else from where you started is.
    let apart = (live - start_live).distance(recorded - start_recorded);
    if apart <= replay.worst {
        return;
    }
    replay.worst = apart;
    // Once a second at most, and only once the gap is wider than a player.
    if apart > 1.0 && replay.at.saturating_sub(replay.said_at) >= 64 {
        replay.said_at = replay.at;
        warn!("replay tick {} has drifted {apart:.2} m from the recording", replay.at);
    }
}
