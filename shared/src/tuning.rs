//! Every network knob, in one place.
//!
//! These are settings, not constants: what a shooter feels like at 30 ms and at 150 ms are
//! different games, and finding out which trade is right means changing a number and playing, not
//! changing a number and waiting for a link step.
//!
//! **A name says who a setting belongs to.** There are three kinds and the prefix is the whole
//! difference:
//!
//! - **`cl_`** — the client's own. Two clients on one server may disagree about every one of them.
//! - **`srv_`** — the server's own. A client reading one would be reading somebody else's business.
//! - **no prefix** — both ends must agree, and **the server decides**. A client fetches these over
//!   the metadata socket before it builds its app and takes them over whatever its own file said;
//!   see [`NetConfig::adopt_from_server`], which is the only place that list lives.
//!
//! The two exceptions are [`port`](NetConfig::port) and [`meta_port`](NetConfig::meta_port):
//! shared, and still not the server's to give, because they are how a client finds one.
//!
//! The link conditioner is why this is worth the letters. It delays only what a process
//! *receives* — the server's copy delays inputs on their way in, each client's copy delays
//! snapshots on their way in — so a hundred milliseconds set on the server alone is a
//! fifty-millisecond half-duplex link that behaves like nothing real, and a run over one looks
//! exactly like a netcode success. It used to be on whoever ran the session to put the same three
//! numbers in two files. Now they are the server's, and a client that could not ask says so.
//!
//! Three layers, each overriding the one before:
//!
//! 1. the defaults below,
//! 2. `noob_tube.toml` — or whatever `NOOB_TUBE_CONFIG` points at,
//! 3. environment variables, one per field, upper-cased and prefixed with `NOOB_TUBE_`.
//!
//! The file is for the settings you keep, the environment for the one you are changing right now:
//! `NOOB_TUBE_PING_MS=200 cargo run -p noob_tube_server` tries one value without editing anything.
//! On a client, a shared setting set either way is overwritten by the server's the moment it
//! answers — which is the point, and which is why the interesting ones are set on the server.
//!
//! ```toml
//! # Both ends. The server's copy wins.
//! tick_hz = 64.0                  # simulation rate
//! ping_ms = 100                   # simulated round trip; each end delays half of it
//! jitter_ms = 10                  # random variation on each leg, ± this
//! loss = 0.02                     # packet loss probability, 0.0 to 1.0
//! lag_compensation = true         # rewind targets to what the shooter saw
//! predict_vehicles = "full"       # how much of a vehicle its driver simulates
//!
//! # Where the server is. Both read them; neither is fetched, for the obvious reason.
//! port = 5000
//! meta_port = 5001
//!
//! # The client's own.
//! cl_cmd_hz = 64.0                # how often it sends inputs
//! cl_input_redundancy = 5         # consecutive input packet losses survived
//! cl_interp_ratio = 1.7           # interpolation delay, in send intervals
//! cl_interp_min_ms = 5            # floor under that delay
//! cl_input_delay_min_ticks = 0    # never act on an input sooner than this
//! cl_input_delay_max_ticks = 0    # latency covered by delay before predicting
//! cl_max_predicted_ticks = 100    # how far it may predict ahead
//! cl_min_lead_ticks = 1.0         # how far its clock is held ahead of the server's
//! cl_jitter_safety_multiple = 4   # how much measured jitter is added to that lead
//!
//! # The server's own.
//! srv_send_hz = 32.0              # how often it replicates
//! srv_lag_comp_history_ticks = 35 # how far back it can rewind
//! srv_edit_delay_ticks = 10       # how late a terrain edit lands
//! ```
//!
//! Both binaries read the same file and each takes the fields it needs, so one file can still
//! describe a whole session — but a client no longer has to be given one for the session to be the
//! session it says it is.

use core::time::Duration;
use std::path::{Path, PathBuf};

use bevy::prelude::Resource;
use lightyear::prelude::*;
use lightyear::prelude::input;
use serde::{Deserialize, Serialize};

/// Where the config is looked for when `NOOB_TUBE_CONFIG` says nothing.
pub const DEFAULT_CONFIG_PATH: &str = "noob_tube.toml";

/// Everything about the network that is worth changing without a rebuild.
///
/// How much of a vehicle a driver's own client works out for itself.
///
/// A ladder rather than a switch, because the interesting setting is the middle one. What a client
/// can compute without being told is its own input and the shape of the level; what it cannot is
/// anything a *different* player influences. [`World`](Self::World) draws the line exactly there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VehiclePrediction {
    /// The driver predicts the vehicle and everything driverless it might hit — loose crates and
    /// parked vehicles are handed to them for the duration, so both sides shove the same box on the
    /// same tick.
    ///
    /// The most responsive and the most fragile. Anything it hits that *is* driven by somebody else
    /// has no answer here at all, because that input belongs to a peer this client never hears from.
    #[default]
    Full,
    /// The driver predicts the vehicle against the level and nothing else.
    ///
    /// The chassis stops colliding with crates and with other vehicles on the driver's own client;
    /// the server still collides with all of it and the correction arrives with the next update.
    /// The trade is deliberate and it is about *when* you pay. Full prediction pays a small,
    /// permanent disagreement on every contact, gentle ones included. This pays nothing at all
    /// while driving — which is nearly all of the time — and pays it in one lump when you crash,
    /// which is the moment a player expects to be thrown around anyway.
    ///
    /// The wheels still find crates: a suspension ray is a query, not a solver contact, and the
    /// server casts the same one. Only the chassis stops noticing them.
    World,
    /// Nobody predicts anything. The vehicle is interpolated like any other scenery, and the input
    /// goes to the server and comes back — measured at 176 ms later at 100 ms of ping.
    Off,
}

impl VehiclePrediction {
    /// Whether a driver's own client simulates the vehicle at all.
    pub fn simulates_the_vehicle(self) -> bool {
        !matches!(self, VehiclePrediction::Off)
    }

    /// Whether that simulation is allowed to touch anything but the level.
    pub fn simulates_contacts(self) -> bool {
        matches!(self, VehiclePrediction::Full)
    }
}

impl core::str::FromStr for VehiclePrediction {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "full" => Ok(Self::Full),
            "world" => Ok(Self::World),
            "off" => Ok(Self::Off),
            other => Err(format!("expected full, world or off, not {other:?}")),
        }
    }
}

/// The keys that were renamed when the prefixes came in, and what they are called now.
///
/// `deny_unknown_fields` turns a file written before them into a refusal to start, which is right.
/// What is not right is serde's own message for it: it names the key it did not know and then lists
/// every key it does, leaving the reader to work out that theirs moved rather than went. This turns
/// that into the one line they need.
///
/// It can go once nobody has a file this old. Until then it is the difference between a rename and
/// a morning.
const RENAMED: [(&str, &str); 12] = [
    ("send_hz", "srv_send_hz"),
    ("lag_comp_history_ticks", "srv_lag_comp_history_ticks"),
    ("edit_delay_ticks", "srv_edit_delay_ticks"),
    ("cmd_hz", "cl_cmd_hz"),
    ("input_redundancy", "cl_input_redundancy"),
    ("interp_ratio", "cl_interp_ratio"),
    ("interp_min_ms", "cl_interp_min_ms"),
    ("input_delay_min_ticks", "cl_input_delay_min_ticks"),
    ("input_delay_max_ticks", "cl_input_delay_max_ticks"),
    ("max_predicted_ticks", "cl_max_predicted_ticks"),
    // The one that lost a word as well as gaining a prefix: `cl_` already says whose lead it is.
    ("min_client_lead_ticks", "cl_min_lead_ticks"),
    ("jitter_safety_multiple", "cl_jitter_safety_multiple"),
];

/// Which of a file's keys have moved, said the way somebody would want to be told.
///
/// Keys only — a line whose *value* happens to read like an old name is not a setting, and neither
/// is anything behind a `#`. Crude on purpose: this runs on the way to a panic, and the worst it
/// can do is name a line that was already going to be pointed at.
fn renamed_in(text: &str) -> Vec<String> {
    let keys: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('=').map(|(key, _)| key.trim()))
        .collect();
    RENAMED
        .iter()
        .filter(|(was, _)| keys.contains(was))
        .map(|(was, now)| format!("{was} is now {now}"))
        .collect()
}

/// Which end of the connection a process is, for the one line it prints about itself.
///
/// It exists because half of [`NetConfig`] means nothing on the wrong end. A client holds a
/// `srv_send_hz` — it read the same file, or took the defaults — and that number has no effect on
/// anything it does; printing it is the same lie as writing a client-only setting into a server's
/// config file, which the config file's own comments warn against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Client,
    Server,
}

/// One value, written for somebody to read rather than for a parser.
///
/// Only floats need it, and only because `loss` is an `f32` widened to an `f64` on its way into
/// TOML: 0.03 comes back as 0.029999999329447746, which in a log line about what changed reads as
/// something having gone wrong with it. Six decimal places is far past anything here is set to.
fn shown(value: &toml::Value) -> String {
    match value {
        toml::Value::Float(f) => ((f * 1.0e6).round() / 1.0e6).to_string(),
        other => other.to_string(),
    }
}

/// `deny_unknown_fields` is deliberate. A misspelled key that silently does nothing is the same
/// failure as a conditioner that was never applied: the run looks fine and every number from it is
/// wrong. Better to refuse to start.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetConfig {
    // ---- What both ends must agree on. The server decides these and a client takes them over the
    // ---- metadata socket before it builds its app; see `adopt_from_server`, which is the only
    // ---- place the list lives. A field down here with no prefix is one of them, and that is the
    // ---- whole rule — the prefixes below exist so that this group can be recognised by not
    // ---- having one.
    /// How often the simulation steps, in Hz.
    ///
    /// The server owns this rate and the client replays prediction at it, so the two **must**
    /// agree. They are separate processes reading separate files, so nothing here can enforce that
    /// — instead [`Self::protocol_id`] mixes the rate into the netcode protocol id, and peers that
    /// disagree simply fail to connect. A refused connection is a bad afternoon; a silent tick-rate
    /// mismatch is a worse week.
    ///
    /// It sets the granularity of everything else: input delay and the client's lead are counted in
    /// ticks, and one tick is `1 / tick_hz`. Raising it costs CPU on the server linearly and makes
    /// each rollback replay more ticks for the same wall-clock error.
    pub tick_hz: f64,
    /// Simulated round trip. Zero leaves the link untouched.
    pub ping_ms: u64,
    /// Random variation added to each leg, in each direction.
    pub jitter_ms: u64,
    /// Packet loss probability, 0.0 to 1.0.
    pub loss: f32,
    /// Whether a shot is tested against where the shooter saw its target.
    ///
    /// On, every shot carries the two received snapshots the client was blending between and how
    /// far between them it was, the client also reports its interpolation delay as a fallback, and
    /// the server rewinds each target into the past before casting the ray — see
    /// [`crate::lag_compensation`]. Off, the server tests against the present, and hitting anything
    /// moving means leading it by the whole round trip.
    ///
    /// Both ends read this, and it means something slightly different on each: the server's history
    /// is useless if no client reports a delay, and a reported delay is ignored if the server keeps
    /// no history.
    pub lag_compensation: bool,
    /// How much of a vehicle a driver's own client works out for itself. See
    /// [`VehiclePrediction`], which carries the reasoning; this is only where it is read from.
    ///
    /// **Both ends read it, and they must agree.** The server decides who gets a `PredictionTarget`
    /// and a client decides what its predicted chassis is allowed to collide with; neither can see
    /// the other's copy. Nothing here enforces it, in the same way nothing enforces the link
    /// conditioner being set on every process.
    pub predict_vehicles: VehiclePrediction,
    // ---- Where the server is. Unprefixed because both binaries read them, and *not* taken from
    // ---- the server for the obvious reason: they are how a client finds one in the first place.
    // ---- The two exceptions to the rule above, and the only two.
    /// UDP port the game itself runs on: the server binds it, the client dials it.
    ///
    /// It exists so that two servers can share a machine, which is the only way to reproduce
    /// anything that needs a session of its own while somebody is playing on the default one. Both
    /// binaries read the same field, so one `NOOB_TUBE_PORT=6000` in front of each is a whole
    /// second session.
    pub port: u16,
    /// TCP port for the metadata endpoint, beside the game's UDP socket.
    ///
    /// The server publishes its config there so a client can adopt the tick rate before building
    /// its app; see [`crate::metadata`]. Zero switches it off — the client then keeps whatever its
    /// own file says, and a disagreement shows up as a refused connection.
    pub meta_port: u16,
    // ---- The client's own. Nothing here reaches the server or has to match anything it does;
    // ---- two clients on one server may disagree about every one of them.
    /// How often the client sends its inputs, in Hz — Source's `cl_cmdrate`.
    ///
    /// Separate from `srv_send_hz`, which is the other direction. Lightyear's own default is to
    /// send every *frame*, which at 200 fps is three input packets per simulated tick: bandwidth
    /// on nothing, since the extra packets carry no tick the previous one did not.
    ///
    /// Matching `tick_hz` is the usual choice and what Source does. Below it, each packet simply
    /// carries the several ticks that accumulated. Zero restores lightyear's every-frame default.
    pub cl_cmd_hz: f64,
    /// How many consecutive lost input packets the client can survive without the server missing a
    /// tick of movement.
    ///
    /// Every input message repeats the last N packets' worth of ticks, so a gap is filled by the
    /// next packet rather than costing movement. It is the cheapest redundancy in the whole
    /// protocol — inputs are a handful of bytes — and the reason a lossy link still walks in a
    /// straight line. Lightyear's default is 5.
    ///
    /// It keeps the *trigger* in step too, which is less obvious. A lost input does not cost a
    /// shot: the server keeps doing the last thing it was told, so it fires a couple of ticks late,
    /// and the cooldown then holds both sides firing at the same rate permanently out of step.
    /// Measured at 50% packet loss, 5 keeps client and server firing on identical ticks and 1 does
    /// not.
    pub cl_input_redundancy: u16,
    /// How far in the past other players are drawn, as a multiple of the send interval.
    ///
    /// Below 1.0 the next update has usually not arrived yet and remote players freeze; well above
    /// it they are drawn needlessly far behind.
    pub cl_interp_ratio: f32,
    /// Floor under the interpolation delay, for when the send rate is very high.
    pub cl_interp_min_ms: u64,
    /// The soonest, in ticks, that the server may act on an input — regardless of how good the
    /// connection is.
    ///
    /// The client stamps each input for tick `now + delay` instead of `now`, so the packet has that
    /// long to arrive before the server needs it. What it buys is fewer rollbacks; what it costs is
    /// that your own movement starts this late, every time, even on a perfect link. Fighting games
    /// and RTSs spend it gladly for a simulation that never rewinds. Shooters generally do not,
    /// which is why this is 0 by default.
    ///
    /// Must not exceed `cl_input_delay_max_ticks`; the two together are a fixed delay.
    pub cl_input_delay_min_ticks: u16,
    /// How much latency is covered by input delay before prediction takes over.
    ///
    /// Below this ping, the delay grows to match the link and there is nothing to roll back; above
    /// it, the delay stops growing and the rest is predicted. Zero means never trade responsiveness
    /// for stability — predict from the first millisecond.
    pub cl_input_delay_max_ticks: u16,
    /// How far ahead of the server the client may simulate.
    ///
    /// This is the ceiling on rollback depth, and therefore on the CPU a correction can cost.
    /// Latency beyond what this covers turns into more input delay instead, up to
    /// `cl_input_delay_max_ticks`. Zero is lockstep: no prediction at all.
    pub cl_max_predicted_ticks: u16,
    /// How far ahead of the server the client's clock is held, in ticks, at minimum.
    ///
    /// This is the other, quieter answer to "when does the server act on my input", and the one
    /// that costs the player nothing. The client's clock already runs ahead of the server's by
    /// about half the ping, precisely so that an input stamped for tick `T` arrives before the
    /// server simulates `T`. This is the guaranteed floor under that lead, on top of what ping and
    /// jitter demand.
    ///
    /// Unlike `cl_input_delay_min_ticks` it does **not** delay local movement: the client applies
    /// its input the moment it is pressed either way. It only moves the whole client timeline further
    /// into the future, so inputs land at the server with more slack.
    ///
    /// Lightyear's default is 1.0 — exactly one tick, the least that can work: below it the server
    /// would sometimes simulate tick `T` before the input for `T` arrived.
    pub cl_min_lead_ticks: f32,
    /// How many multiples of the measured jitter to add to the client's lead.
    ///
    /// The margin covers `jitter × this`, so it is a bet on how many packets arrive in time: 1
    /// covers about 65%, 2 about 95%, 3 about 99.7%. Lightyear defaults to 4.
    pub cl_jitter_safety_multiple: u8,
    // ---- The server's own. A client reading these would be reading somebody else's business:
    // ---- the send rate arrives over the wire as `SenderMetadata`, and the other two are things
    // ---- the server does on its own behalf and stamps into what it sends.
    /// How often the server sends replication updates.
    ///
    /// Deliberately below the tick rate. Lightyear's own default is zero, meaning an update every
    /// frame — more bandwidth than a shooter needs, and it hides the problem interpolation exists
    /// to solve: with no gap between updates there is nothing to interpolate across, so remote
    /// players look smooth for the wrong reason and start stepping the moment the rate drops.
    pub srv_send_hz: f64,
    /// How many ticks of player positions the server keeps to rewind into.
    ///
    /// This is the ceiling on how far a shot may reach back, so it must cover the worst connection
    /// the server means to serve: round trip plus interpolation delay. 35 ticks is about 550 ms at
    /// 64 Hz, which is generous — the memory is a few hundred bytes per player.
    ///
    /// Too short is not silent: the server logs a rewind it could not satisfy rather than quietly
    /// testing against a position the shooter never saw.
    pub srv_lag_comp_history_ticks: u16,
    /// How many ticks after committing a terrain edit everybody applies it.
    ///
    /// The server stamps every accepted stroke with `now + this` and both sides apply it there, so
    /// the ground moves at one tick for everyone. It is a margin, and it has to cover two different
    /// things at once:
    ///
    /// 1. **The commit has to arrive first.** One trip from the server to the slowest client, plus
    ///    jitter. Below that, a client is told to apply an edit at a tick it has already passed.
    /// 2. **No rollback window may straddle it.** `Level` reads Avian's *current* spatial state,
    ///    with no seam where a replay could be handed historical terrain — so if the edit lands
    ///    inside the window a client is predicting over, the replayed ticks from before the edit
    ///    are walked on the ground from after it.
    ///
    /// The first version of this used `cl_max_predicted_ticks`, which satisfies (2) by construction
    /// and costs **1.5 seconds** at the defaults — the ceiling on how far a client may *ever*
    /// predict, paid on every stroke, when the window a client is actually predicting over is a
    /// handful of ticks. Ten ticks is 156 ms at 64 Hz, and it is what `webgame` uses for the same
    /// job at 100 Hz for reason (1) alone.
    ///
    /// What (2) then buys is a probability rather than a guarantee: a client whose lead has grown
    /// past this — a bad link, a long stall — may take one rollback that straddles an edit, and be
    /// corrected by however far the ground moved under it. Small, rare, and self-correcting, which
    /// is the trade the alternative was refusing to make at fifteen times the cost.
    pub srv_edit_delay_ticks: u16,

}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            // What CS:GO's official servers run. Third-party competitive servers use 128.
            tick_hz: 64.0,
            ping_ms: 0,
            jitter_ms: 0,
            loss: 0.0,
            srv_send_hz: 32.0,
            // Matching the tick rate, as Source does with cl_cmdrate.
            cl_cmd_hz: 64.0,
            cl_input_redundancy: 5,
            cl_interp_ratio: 1.7,
            cl_interp_min_ms: 5,
            // Lightyear's `no_input_delay()`: cover every millisecond of latency with prediction.
            cl_input_delay_min_ticks: 0,
            cl_input_delay_max_ticks: 0,
            cl_max_predicted_ticks: 100,
            // Lightyear's `SyncConfig` defaults.
            cl_min_lead_ticks: 1.0,
            cl_jitter_safety_multiple: 4,
            lag_compensation: true,
            // ~550 ms at 64 Hz: past any playable connection, and cheap.
            srv_lag_comp_history_ticks: 35,
            // 156 ms at 64 Hz. `webgame` uses ten ticks for the same job.
            srv_edit_delay_ticks: 10,
            // The behaviour everything so far was measured against.
            predict_vehicles: VehiclePrediction::Full,
            port: crate::SERVER_PORT,
            // Beside the game port, which is UDP; the two do not collide.
            meta_port: crate::SERVER_PORT + 1,
        }
    }
}

impl NetConfig {
    /// Reads the file if there is one, then lets the environment override it.
    ///
    /// A missing file is normal and silent — the defaults are a playable configuration. A file that
    /// exists but cannot be read or parsed is fatal, because the alternative is starting with
    /// settings the operator did not ask for and cannot see.
    pub fn load() -> Self {
        let mut config = match config_path() {
            Some(path) => Self::from_file(&path),
            None => Self::default(),
        };
        config.apply_env();
        config.follow_the_game_port();
        config.validate();
        config
    }

    /// Moves the metadata port with the game port, when it was sitting just above it.
    ///
    /// Two servers on one machine is the whole reason [`port`](Self::port) exists, and a second one
    /// that took the first's *metadata* socket would be exactly as unusable as one that took its
    /// game socket. So moving one number is enough. An operator who has placed the metadata port
    /// somewhere of their own keeps it there, and zero — which switches it off — is left alone,
    /// because neither is "just above the game port".
    fn follow_the_game_port(&mut self) {
        if self.port != crate::SERVER_PORT && self.meta_port == crate::SERVER_PORT + 1 {
            self.meta_port = self.port + 1;
        }
    }

    /// Rejects combinations that cannot mean anything, before lightyear asserts on them from
    /// inside a system where the message is far less useful.
    fn validate(&self) {
        assert!(
            self.tick_hz > 0.0,
            "tick_hz is {}, which is not a rate",
            self.tick_hz,
        );
        assert!(
            self.cl_min_lead_ticks >= 1.0,
            "cl_min_lead_ticks is {}, below one tick: the server would sometimes simulate a \
             tick before the input for it had arrived.",
            self.cl_min_lead_ticks,
        );
        assert!(
            self.cl_input_delay_min_ticks <= self.cl_input_delay_max_ticks,
            "cl_input_delay_min_ticks ({}) exceeds cl_input_delay_max_ticks ({}): a floor above \
             the ceiling is not a setting. For a fixed delay of n ticks, set both to n.",
            self.cl_input_delay_min_ticks,
            self.cl_input_delay_max_ticks,
        );
        assert!(
            !self.lag_compensation || self.srv_lag_comp_history_ticks > 0,
            "lag_compensation is on with srv_lag_comp_history_ticks = 0: there would be no past to \
             rewind into. Set a history length, or turn lag compensation off.",
        );
    }

    fn from_file(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        toml::from_str(&text).unwrap_or_else(|err| {
            let moved = renamed_in(&text);
            if moved.is_empty() {
                panic!("cannot parse {}: {err}", path.display());
            }
            panic!(
                "cannot parse {}: {err}\n\nA setting only one end reads now says which end that \
                 is:\n  {}",
                path.display(),
                moved.join("\n  "),
            )
        })
    }

    fn apply_env(&mut self) {
        env_parse("NOOB_TUBE_TICK_HZ", &mut self.tick_hz);
        env_parse("NOOB_TUBE_PING_MS", &mut self.ping_ms);
        env_parse("NOOB_TUBE_JITTER_MS", &mut self.jitter_ms);
        env_parse("NOOB_TUBE_LOSS", &mut self.loss);
        env_parse("NOOB_TUBE_SRV_SEND_HZ", &mut self.srv_send_hz);
        env_parse("NOOB_TUBE_CL_CMD_HZ", &mut self.cl_cmd_hz);
        env_parse("NOOB_TUBE_CL_INPUT_REDUNDANCY", &mut self.cl_input_redundancy);
        env_parse("NOOB_TUBE_CL_INTERP_RATIO", &mut self.cl_interp_ratio);
        env_parse("NOOB_TUBE_CL_INTERP_MIN_MS", &mut self.cl_interp_min_ms);
        env_parse("NOOB_TUBE_CL_INPUT_DELAY_MIN_TICKS", &mut self.cl_input_delay_min_ticks);
        env_parse("NOOB_TUBE_CL_INPUT_DELAY_MAX_TICKS", &mut self.cl_input_delay_max_ticks);
        env_parse("NOOB_TUBE_CL_MAX_PREDICTED_TICKS", &mut self.cl_max_predicted_ticks);
        env_parse("NOOB_TUBE_CL_MIN_LEAD_TICKS", &mut self.cl_min_lead_ticks);
        env_parse("NOOB_TUBE_CL_JITTER_SAFETY_MULTIPLE", &mut self.cl_jitter_safety_multiple);
        env_parse("NOOB_TUBE_PORT", &mut self.port);
        env_parse("NOOB_TUBE_META_PORT", &mut self.meta_port);
        env_parse("NOOB_TUBE_LAG_COMPENSATION", &mut self.lag_compensation);
        env_parse("NOOB_TUBE_SRV_LAG_COMP_HISTORY_TICKS", &mut self.srv_lag_comp_history_ticks);
        env_parse("NOOB_TUBE_SRV_EDIT_DELAY_TICKS", &mut self.srv_edit_delay_ticks);
        env_parse("NOOB_TUBE_PREDICT_VEHICLES", &mut self.predict_vehicles);
    }

    /// The link conditioner for this process, or `None` when nothing is being simulated.
    ///
    /// Each end delays only what it receives, so it gets **half** the ping: the two halves add up
    /// to the round trip that was asked for. Getting this wrong in either direction is the easiest
    /// mistake here — a "100 ms ping" that is really 200 makes every later number wrong.
    pub fn conditioner(&self) -> Option<RecvLinkConditioner> {
        if self.ping_ms == 0 && self.jitter_ms == 0 && self.loss <= 0.0 {
            return None;
        }
        let config = LinkConditionerConfig::default()
            .with_incoming_latency(Duration::from_millis(self.ping_ms) / 2)
            .with_incoming_jitter(Duration::from_millis(self.jitter_ms))
            .with_fixed_loss(self.loss.clamp(0.0, 1.0));
        Some(RecvLinkConditioner::new(config))
    }

    /// Takes over everything the two ends have to agree about.
    ///
    /// **This is the list, and there is nowhere else it is written down.** A field of [`NetConfig`]
    /// is in exactly one of four states, and which one is legible from its name:
    ///
    /// - **no prefix, and here** — the server decides it and a client takes it. Disagreement is
    ///   either a desync or a run whose numbers are all wrong, and the client cannot know which.
    /// - **no prefix, and not here** — [`port`](Self::port) and [`meta_port`](Self::meta_port),
    ///   which cannot come from the server because they are how a client finds one.
    /// - **`cl_`** — the client's own, and none of the server's business.
    /// - **`srv_`** — the server's own, and none of the client's.
    ///
    /// `the_prefixes_say_who_decides` holds that rule up against the struct by its field *names*,
    /// so a field added without a decision fails a test rather than quietly joining whichever group
    /// it was declared next to.
    ///
    /// The link conditioner is the case that shows why this matters more than the tick rate does.
    /// It delays only what a process *receives* — the server's copy delays inputs coming in, each
    /// client's copy delays snapshots coming in — so a hundred milliseconds set on the server alone
    /// is a fifty-millisecond half-duplex link that behaves like nothing real. It used to be on
    /// every operator to set the same numbers in two files. Now the server's are the numbers.
    ///
    /// A client that could not reach the endpoint keeps its own and says so; see the caller. The
    /// tick rate is caught anyway by [`protocol_id`](Self::protocol_id) refusing the connection,
    /// and nothing catches the rest.
    /// Returns what actually moved, as `key was -> is`, so the caller can say it out loud without
    /// keeping a second copy of the list that would go stale the first time this one changed.
    pub fn adopt_from_server(&mut self, server: &NetConfig) -> Vec<String> {
        let before = *self;
        self.tick_hz = server.tick_hz;
        self.ping_ms = server.ping_ms;
        self.jitter_ms = server.jitter_ms;
        self.loss = server.loss;
        self.lag_compensation = server.lag_compensation;
        self.predict_vehicles = server.predict_vehicles;
        before.changes_to(self)
    }

    /// Which fields two configs disagree about, written the way a log line wants them.
    ///
    /// Through serde rather than field by field, for the reason everything else about this rule is:
    /// a field added next month is covered without anybody remembering to come back here. A config
    /// that will not serialise is not worth a panic on the startup path — the settings are in
    /// effect either way, and the only loss is a line saying so.
    fn changes_to(&self, now: &NetConfig) -> Vec<String> {
        let (Ok(was), Ok(is)) = (toml::Value::try_from(self), toml::Value::try_from(now)) else {
            return Vec::new();
        };
        let (Some(was), Some(is)) = (was.as_table(), is.as_table()) else {
            return Vec::new();
        };
        was.iter()
            .filter(|(key, before)| is.get(*key) != Some(before))
            .map(|(key, before)| format!("{key} {} -> {}", shown(before), shown(&is[key])))
            .collect()
    }

    /// One tick.
    pub fn tick_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.tick_hz)
    }

    /// How long the server's main loop sleeps between frames.
    ///
    /// Without one it does not sleep at all. `MinimalPlugins` brings a `ScheduleRunnerPlugin` set
    /// to `wait: None`, which is a loop that runs as fast as the machine allows — measured on this
    /// one, a headless server with **nobody connected** sat at 99.9% of a core, doing thousands of
    /// empty frames a second for a simulation that advances sixty-four times.
    ///
    /// That is not merely waste. A core spinning flat out is a core competing with everything else
    /// on the machine, the game's own client included, and the price is paid where it is hardest to
    /// read: as jitter in the moment each tick actually runs.
    ///
    /// **Twice the tick rate**, not once. At exactly the tick rate the loop and the fixed timestep
    /// beat against each other — a frame that arrives a hair early runs no tick and the next runs
    /// two, which is the same tick timing being unsteady, arrived at from the other side. At double
    /// it, every tick lands within half a frame of when it is due, and a message waits at most
    /// eight milliseconds to be looked at, against a send rate of thirty-two a second.
    pub fn frame_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / (2.0 * self.tick_hz.max(f64::MIN_POSITIVE)))
    }

    /// The netcode protocol id, with the tick rate mixed in.
    ///
    /// Netcode refuses a connect token whose protocol id does not match, which turns a tick-rate
    /// disagreement from an invisible desync into a connection that never establishes. The two
    /// processes read their config separately and nothing else can catch this.
    pub fn protocol_id(&self) -> u64 {
        crate::PROTOCOL_ID ^ self.tick_hz.to_bits().rotate_left(17)
    }

    /// How often the server sends replication updates.
    pub fn send_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.srv_send_hz.max(f64::MIN_POSITIVE))
    }

    /// How often the client sends inputs, and how much history each packet repeats.
    ///
    /// Zero `cl_cmd_hz` means every frame, which is lightyear's own default and what
    /// `Duration::default()` means to it.
    pub fn input_config(&self) -> input::InputConfig<crate::player::PlayerInput> {
        input::InputConfig {
            send_interval: if self.cl_cmd_hz > 0.0 {
                Duration::from_secs_f64(1.0 / self.cl_cmd_hz)
            } else {
                Duration::default()
            },
            packet_redundancy: self.cl_input_redundancy,
            // Makes the client report its interpolation delay with every input message. Without it
            // the server has no way to know how far into the past to rewind, and lag compensation
            // is off whatever the server does.
            lag_compensation: self.lag_compensation,
            ..Default::default()
        }
    }

    /// The delay a client draws other players at.
    pub fn interpolation(&self) -> InterpolationConfig {
        InterpolationConfig::default()
            .with_send_interval_ratio(self.cl_interp_ratio)
            .with_min_delay(Duration::from_millis(self.cl_interp_min_ms))
    }

    /// How far ahead of the present a client stamps its inputs, and how far it may predict.
    ///
    /// Client-side only: the server acts on whatever tick an input is stamped for, and has no say
    /// in the choice.
    pub fn input_timeline(&self) -> InputTimelineConfig {
        InputTimelineConfig::default()
            .with_input_delay(client::InputDelayConfig {
                minimum_input_delay_ticks: self.cl_input_delay_min_ticks,
                maximum_input_delay_before_prediction: self.cl_input_delay_max_ticks,
                maximum_predicted_ticks: self.cl_max_predicted_ticks,
            })
            .with_sync_config(SyncConfig {
                jitter_margin: self.cl_min_lead_ticks,
                jitter_multiple: self.cl_jitter_safety_multiple,
                ..SyncConfig::default()
            })
    }

    /// One line describing what is actually in effect, for the log.
    ///
    /// The shared settings, then whichever end's own settings the caller is. Printed
    /// unconditionally rather than only when something is set, because "link untouched" is the fact
    /// most worth stating: a run that was supposed to be lagged and silently was not looks exactly
    /// like a netcode success.
    pub fn describe(&self, side: Side) -> String {
        let link = if self.conditioner().is_some() {
            format!(
                "ping {} ms, jitter ±{} ms per leg, loss {}",
                self.ping_ms, self.jitter_ms, self.loss
            )
        } else {
            "link untouched".to_string()
        };
        let source = match config_path() {
            Some(path) => path.display().to_string(),
            None => "defaults".to_string(),
        };
        let lag = match (self.lag_compensation, side) {
            (false, _) => "no lag compensation".to_string(),
            // How far back is the server's own, and a client holding a number for it holds one
            // nothing will ever rewind by.
            (true, Side::Client) => "lag compensation on".to_string(),
            (true, Side::Server) => {
                format!("lag compensation over {} ticks", self.srv_lag_comp_history_ticks)
            }
        };
        let stale = if std::env::var("NOOB_TUBE_LATENCY_MS").is_ok() {
            " (ignoring NOOB_TUBE_LATENCY_MS, renamed to NOOB_TUBE_PING_MS)"
        } else {
            ""
        };
        let ours = match side {
            Side::Client => format!(
                "inputs at {:.0} Hz ×{}, interpolating at {}×, input delay {}..{} ticks, \
                 predicting up to {}, lead ≥{} ticks",
                self.cl_cmd_hz,
                self.cl_input_redundancy,
                self.cl_interp_ratio,
                self.cl_input_delay_min_ticks,
                self.cl_input_delay_max_ticks,
                self.cl_max_predicted_ticks,
                self.cl_min_lead_ticks,
            ),
            Side::Server => format!(
                "sending at {:.0} Hz, edits landing {} ticks late",
                self.srv_send_hz, self.srv_edit_delay_ticks,
            ),
        };
        format!(
            "{link}, ticking at {:.0} Hz, {ours}, {lag} [{source}]{stale}",
            self.tick_hz,
        )
    }
}

/// The config file to read, if there is one.
///
/// An explicit `NOOB_TUBE_CONFIG` that does not exist is an error rather than a fallback: it was
/// named on purpose, and quietly running with different settings than the ones pointed at is worse
/// than not starting.
fn config_path() -> Option<PathBuf> {
    match std::env::var("NOOB_TUBE_CONFIG") {
        Ok(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.exists(),
                "NOOB_TUBE_CONFIG points at {}, which does not exist",
                path.display()
            );
            Some(path)
        }
        Err(_) => {
            let path = PathBuf::from(DEFAULT_CONFIG_PATH);
            path.exists().then_some(path)
        }
    }
}

/// Overwrites `slot` if the variable is set and parses. A set-but-unparsable value is fatal, for
/// the same reason a broken config file is.
fn env_parse<T: std::str::FromStr>(name: &str, slot: &mut T)
where
    T::Err: core::fmt::Display,
{
    let Ok(raw) = std::env::var(name) else { return };
    match raw.parse() {
        Ok(value) => *slot = value,
        Err(err) => panic!("{name}={raw} is not a valid value: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_end_delays_half_the_ping() {
        let config = NetConfig { ping_ms: 100, ..default_config() };
        // Not directly readable from the conditioner, so this asserts the arithmetic that produces
        // it. The trap it guards is a one-way value being read as a round trip, or the reverse.
        assert_eq!(Duration::from_millis(config.ping_ms) / 2, Duration::from_millis(50));
        assert!(config.conditioner().is_some());
    }

    #[test]
    fn an_untouched_link_gets_no_conditioner() {
        assert!(default_config().conditioner().is_none());
    }

    #[test]
    fn loss_alone_is_enough_to_condition_the_link() {
        let config = NetConfig { loss: 0.05, ..default_config() };
        assert!(config.conditioner().is_some());
    }

    #[test]
    fn send_hz_becomes_an_interval() {
        let config = NetConfig { srv_send_hz: 32.0, ..default_config() };
        assert_eq!(config.send_interval(), Duration::from_secs_f64(1.0 / 32.0));
    }

    /// A file may set any subset; the rest stays at its default.
    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let config: NetConfig = toml::from_str("ping_ms = 120").unwrap();
        assert_eq!(config.ping_ms, 120);
        assert_eq!(config.srv_send_hz, NetConfig::default().srv_send_hz);
    }

    /// The whole point of `deny_unknown_fields`: a typo must not read as "no latency".
    #[test]
    fn a_misspelled_key_is_refused() {
        assert!(toml::from_str::<NetConfig>("pign_ms = 120").is_err());
    }

    #[test]
    fn no_input_delay_by_default() {
        let config = default_config();
        assert_eq!(config.cl_input_delay_min_ticks, 0);
        assert_eq!(config.cl_input_delay_max_ticks, 0);
    }

    #[test]
    fn a_fixed_delay_sets_both_ends() {
        let config = NetConfig {
            cl_input_delay_min_ticks: 3,
            cl_input_delay_max_ticks: 3,
            ..default_config()
        };
        config.validate();
        // `InputTimelineConfig` keeps its fields private, so this asserts that the pair we hand
        // lightyear is the one that means "always three ticks, on any link".
        assert_eq!(config.cl_input_delay_min_ticks, config.cl_input_delay_max_ticks);
        let _ = config.input_timeline();
    }

    /// A floor above the ceiling would otherwise reach lightyear and assert from inside a system.
    #[test]
    #[should_panic(expected = "cl_input_delay_min_ticks")]
    fn a_floor_above_the_ceiling_is_refused() {
        NetConfig {
            cl_input_delay_min_ticks: 5,
            cl_input_delay_max_ticks: 2,
            ..default_config()
        }
        .validate();
    }

    /// Lag compensation with nothing to rewind into would look enabled and do nothing, which is
    /// the failure mode this whole file is arranged against.
    #[test]
    #[should_panic(expected = "no past")]
    fn lag_compensation_without_a_history_is_refused() {
        NetConfig {
            lag_compensation: true,
            srv_lag_comp_history_ticks: 0,
            ..default_config()
        }
        .validate();
    }

    /// Turning it off is allowed to zero the history — there is then nothing to keep.
    #[test]
    fn no_lag_compensation_needs_no_history() {
        NetConfig {
            lag_compensation: false,
            srv_lag_comp_history_ticks: 0,
            ..default_config()
        }
        .validate();
    }

    /// The server's history is worthless unless the client reports its delay, and that report is
    /// switched on here and nowhere else.
    #[test]
    fn lag_compensation_reaches_the_input_plugin() {
        let on = NetConfig { lag_compensation: true, ..default_config() };
        let off = NetConfig { lag_compensation: false, ..default_config() };
        assert!(on.input_config().lag_compensation);
        assert!(!off.input_config().lag_compensation);
    }

    /// The two answers to "when does the server act on my input" are independent, and only one of
    /// them costs the player anything.
    #[test]
    fn the_client_lead_is_not_input_delay() {
        let lead = NetConfig { cl_min_lead_ticks: 6.0, ..default_config() };
        lead.validate();
        assert_eq!(lead.cl_input_delay_min_ticks, 0, "raising the lead must not delay local input");
    }

    #[test]
    #[should_panic(expected = "cl_min_lead_ticks")]
    fn a_lead_below_one_tick_is_refused() {
        NetConfig { cl_min_lead_ticks: 0.5, ..default_config() }.validate();
    }

    /// The whole point of the metadata endpoint, and its whole limit: what has to match crosses,
    /// what is the client's own does not.
    #[test]
    fn what_has_to_match_is_the_server_s_to_say() {
        let server = NetConfig {
            tick_hz: 128.0,
            ping_ms: 300,
            cl_cmd_hz: 20.0,
            cl_interp_ratio: 4.0,
            srv_send_hz: 8.0,
            ..default_config()
        };
        let mut client = NetConfig { tick_hz: 64.0, ping_ms: 50, ..default_config() };

        client.adopt_from_server(&server);

        assert_eq!(client.tick_hz, 128.0, "the tick rate has to cross");
        assert_eq!(client.ping_ms, 300, "the server's simulated link has to cross");
        assert_eq!(client.cl_cmd_hz, default_config().cl_cmd_hz, "our input rate is ours");
        assert_eq!(client.cl_interp_ratio, default_config().cl_interp_ratio, "so is our delay");
        let mine = default_config().srv_send_hz;
        assert_eq!(client.srv_send_hz, mine, "and the server's send rate is its own business");
    }

    /// A client takes the server's simulated link, which nobody has to copy into two files now.
    ///
    /// The case worth its own test because it is the one the old rule got wrong and could not
    /// catch. The conditioner delays only what a process *receives*, so a server lagged alone is a
    /// half-duplex link — and a run over one looks like a netcode success that was never tested.
    #[test]
    fn the_server_decides_what_the_link_looks_like() {
        let server = NetConfig { ping_ms: 200, jitter_ms: 30, loss: 0.1, ..default_config() };
        let mut client = NetConfig { ping_ms: 0, jitter_ms: 0, loss: 0.0, ..default_config() };
        assert!(client.conditioner().is_none(), "the client starts on a clean link");

        client.adopt_from_server(&server);

        assert!(client.conditioner().is_some(), "the server's link never reached the client");
        assert_eq!((client.ping_ms, client.jitter_ms, client.loss), (200, 30, 0.1));
    }

    /// A file written before the prefixes is told what to do about it, not merely refused.
    ///
    /// Every renamed key is in the list, and nothing else is — a commented-out line is not a
    /// setting, and neither is a value that happens to read like one.
    #[test]
    fn an_old_file_is_told_what_its_keys_are_called_now() {
        let said = renamed_in("send_hz = 32.0\n#cmd_hz = 64.0\ntick_hz = 64.0\n");
        assert_eq!(said, vec!["send_hz is now srv_send_hz".to_string()]);

        // Every rename is findable, which is the only way the list is worth having.
        for (was, now) in RENAMED {
            let said = renamed_in(&format!("{was} = 1\n"));
            assert_eq!(said, vec![format!("{was} is now {now}")], "{was} was not recognised");
        }

        // And a file that is simply broken says nothing about renames.
        assert!(renamed_in("tick_hz = [").is_empty());
    }

    /// Every renamed key really is gone, and every name it moved to really is there.
    ///
    /// The list is hand-written and the struct is not, so this is what stops it drifting into
    /// advice about a field that no longer exists — or worse, advice to use a name nothing reads.
    #[test]
    fn the_renames_point_at_keys_that_exist() {
        let table = toml::Value::try_from(NetConfig::default()).expect("a config serialises");
        let table = table.as_table().expect("into a table");
        for (was, now) in RENAMED {
            assert!(!table.contains_key(was), "{was} was renamed and is still a key");
            assert!(table.contains_key(now), "{was} was renamed to {now}, which is not a key");
        }
    }

    /// The prefixes *are* the rule, and this is what holds the struct to them.
    ///
    /// Every field is one of four things and its name says which: no prefix means the server
    /// decides it, `cl_` means the client does, `srv_` means the server keeps it to itself, and
    /// `port` and `meta_port` are the two shared ones that still cannot come from a server, because
    /// they are how a client finds one.
    ///
    /// Read through serde rather than field by field, which is the point: a field added next month
    /// is covered by this without anybody remembering to come back here, and a field added to the
    /// struct without a decision about who owns it fails here rather than quietly joining whichever
    /// group it happened to be declared next to.
    #[test]
    fn the_prefixes_say_who_decides() {
        // The two exceptions, and the only two.
        const ADDRESSES: [&str; 2] = ["port", "meta_port"];

        /// Any value, changed into a different one of the same kind.
        ///
        /// So that "the server's value crossed" and "ours stayed" are different claims about every
        /// field. Building a server config that differs everywhere by hand would be the field-by-
        /// field list this test exists to avoid keeping.
        fn nudged(value: &toml::Value) -> toml::Value {
            match value {
                toml::Value::Integer(n) => toml::Value::Integer(n + 1),
                toml::Value::Float(f) => toml::Value::Float(f + 1.0),
                toml::Value::Boolean(b) => toml::Value::Boolean(!b),
                // The one enum in here. Any other variant will do; it only has to differ.
                toml::Value::String(s) if s == "full" => toml::Value::String("world".into()),
                other => panic!("nothing here knows how to change a {other:?}"),
            }
        }

        let ours = NetConfig::default();
        let plain = toml::Value::try_from(ours).expect("a config serialises");
        let mut table = plain.as_table().expect("into a table").clone();
        for (_, value) in table.iter_mut() {
            *value = nudged(value);
        }
        let server: NetConfig = table.clone().try_into().expect("a changed config is still one");

        let mut adopted = ours;
        adopted.adopt_from_server(&server);
        let before = plain.as_table().expect("into a table");
        let after = toml::Value::try_from(adopted).expect("a config serialises");
        let after = after.as_table().expect("into a table");

        for (key, theirs) in &table {
            let mine = &before[key];
            assert_ne!(theirs, mine, "{key} was not changed, so nothing about it is tested");
            let the_server_s = !key.starts_with("cl_")
                && !key.starts_with("srv_")
                && !ADDRESSES.contains(&key.as_str());
            match the_server_s {
                true => assert_eq!(
                    &after[key], theirs,
                    "{key} has no prefix, so the server decides it -- and it did not cross",
                ),
                false => assert_eq!(
                    &after[key], &before[key],
                    "{key} is not the server's to give, and it crossed anyway",
                ),
            }
        }
    }

    /// The endpoint speaks the same TOML the config file does, so a round trip must be lossless.
    #[test]
    fn the_config_survives_a_round_trip() {
        let original =
            NetConfig { tick_hz: 128.0, ping_ms: 70, cl_cmd_hz: 20.0, ..default_config() };
        let text = toml::to_string(&original).unwrap();
        assert_eq!(toml::from_str::<NetConfig>(&text).unwrap(), original);
    }

    #[test]
    fn a_command_rate_becomes_an_interval() {
        let config = NetConfig { cl_cmd_hz: 64.0, ..default_config() };
        assert_eq!(
            config.input_config().send_interval,
            Duration::from_secs_f64(1.0 / 64.0)
        );
    }

    /// Zero is lightyear's "every frame", which `Duration::default()` is how it spells.
    #[test]
    fn a_command_rate_of_zero_means_every_frame() {
        let config = NetConfig { cl_cmd_hz: 0.0, ..default_config() };
        assert_eq!(config.input_config().send_interval, Duration::default());
    }

    #[test]
    fn a_tick_rate_becomes_a_duration() {
        let config = NetConfig { tick_hz: 64.0, ..default_config() };
        assert_eq!(config.tick_duration(), Duration::from_secs_f64(1.0 / 64.0));
    }

    /// The whole reason the tick rate is in the protocol id: two peers that disagree must not be
    /// able to connect and then quietly disagree about everything else.
    #[test]
    fn a_different_tick_rate_is_a_different_protocol() {
        let a = NetConfig { tick_hz: 64.0, ..default_config() };
        let b = NetConfig { tick_hz: 128.0, ..default_config() };
        assert_ne!(a.protocol_id(), b.protocol_id());
    }

    /// ...but nothing else may change it, or unrelated settings would stop peers connecting.
    #[test]
    fn other_settings_do_not_change_the_protocol() {
        let a = default_config();
        let b = NetConfig { ping_ms: 250, srv_send_hz: 20.0, ..default_config() };
        assert_eq!(a.protocol_id(), b.protocol_id());
    }

    #[test]
    #[should_panic(expected = "tick_hz")]
    fn a_tick_rate_of_zero_is_refused() {
        NetConfig { tick_hz: 0.0, ..default_config() }.validate();
    }

    fn default_config() -> NetConfig {
        NetConfig::default()
    }
}
