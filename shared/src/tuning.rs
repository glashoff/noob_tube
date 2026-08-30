//! Every network knob, in one place.
//!
//! These are settings, not constants: what a shooter feels like at 30 ms and at 150 ms are
//! different games, and finding out which trade is right means changing a number and playing, not
//! changing a number and waiting for a link step.
//!
//! Three layers, each overriding the one before:
//!
//! 1. the defaults below,
//! 2. `noob_tube.toml` — or whatever `NOOB_TUBE_CONFIG` points at,
//! 3. environment variables, one per field.
//!
//! The file is for the settings you keep, the environment for the one you are changing right now:
//! `NOOB_TUBE_PING_MS=200 cargo run -p noob_tube_client` tries one value without editing anything.
//!
//! ```toml
//! tick_hz = 64.0       # simulation rate; must match on both sides
//! ping_ms = 100        # simulated round trip; each end delays half of it
//! jitter_ms = 10       # random variation on each leg, ± this
//! loss = 0.02          # packet loss probability, 0.0 to 1.0
//! send_hz = 32.0            # how often the server replicates          (server only)
//! cmd_hz = 64.0             # how often the client sends inputs       (client only)
//! input_redundancy = 5      # consecutive input packet losses survived (client only)
//! interp_ratio = 1.7        # interpolation delay, in send intervals   (client only)
//! interp_min_ms = 5         # floor under that delay                   (client only)
//! input_delay_min_ticks = 0 # never process an input sooner than this  (client only)
//! input_delay_max_ticks = 0 # latency covered by delay before predicting (client only)
//! max_predicted_ticks = 100 # how far the client may predict ahead     (client only)
//! lag_compensation = true   # rewind targets to what the shooter saw      (both)
//! lag_comp_history_ticks = 35 # how far back the server can rewind       (server only)
//! ```
//!
//! Both binaries read the same file and each takes the fields it needs, so one file describes a
//! whole session.
//!
//! The conditioner has to be in effect on **every process**, because it delays only what a process
//! *receives*: the server's copy delays inputs on their way in, each client's copy delays snapshots
//! on their way in. Configure only the server and you get a half-duplex link that behaves like
//! nothing real.

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
/// `deny_unknown_fields` is deliberate. A misspelled key that silently does nothing is the same
/// failure as a conditioner that was never applied: the run looks fine and every number from it is
/// wrong. Better to refuse to start.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetConfig {
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
    /// How often the server sends replication updates.
    ///
    /// Deliberately below the tick rate. Lightyear's own default is zero, meaning an update every
    /// frame — more bandwidth than a shooter needs, and it hides the problem interpolation exists
    /// to solve: with no gap between updates there is nothing to interpolate across, so remote
    /// players look smooth for the wrong reason and start stepping the moment the rate drops.
    pub send_hz: f64,
    /// How often the client sends its inputs, in Hz — Source's `cl_cmdrate`.
    ///
    /// Separate from `send_hz`, which is the other direction. Lightyear's own default is to send
    /// every *frame*, which at 200 fps is three input packets per simulated tick: bandwidth spent
    /// on nothing, since the extra packets carry no tick the previous one did not.
    ///
    /// Matching `tick_hz` is the usual choice and what Source does. Below it, each packet simply
    /// carries the several ticks that accumulated. Zero restores lightyear's every-frame default.
    pub cmd_hz: f64,
    /// How many consecutive lost input packets the client can survive without the server missing a
    /// tick of movement.
    ///
    /// Every input message repeats the last N packets' worth of ticks, so a gap is filled by the
    /// next packet rather than costing movement. It is the cheapest redundancy in the whole
    /// protocol — inputs are a handful of bytes — and the reason a lossy link still walks in a
    /// straight line. Lightyear's default is 5.
    pub input_redundancy: u16,
    /// How far in the past other players are drawn, as a multiple of the send interval.
    ///
    /// Below 1.0 the next update has usually not arrived yet and remote players freeze; well above
    /// it they are drawn needlessly far behind.
    pub interp_ratio: f32,
    /// Floor under the interpolation delay, for when the send rate is very high.
    pub interp_min_ms: u64,
    /// The soonest, in ticks, that the server may act on an input — regardless of how good the
    /// connection is.
    ///
    /// The client stamps each input for tick `now + delay` instead of `now`, so the packet has that
    /// long to arrive before the server needs it. What it buys is fewer rollbacks; what it costs is
    /// that your own movement starts this late, every time, even on a perfect link. Fighting games
    /// and RTSs spend it gladly for a simulation that never rewinds. Shooters generally do not,
    /// which is why this is 0 by default.
    ///
    /// Must not exceed `input_delay_max_ticks`; the two together are a fixed delay.
    pub input_delay_min_ticks: u16,
    /// How much latency is covered by input delay before prediction takes over.
    ///
    /// Below this ping, the delay grows to match the link and there is nothing to roll back; above
    /// it, the delay stops growing and the rest is predicted. Zero means never trade responsiveness
    /// for stability — predict from the first millisecond.
    pub input_delay_max_ticks: u16,
    /// How far ahead of the server the client may simulate.
    ///
    /// This is the ceiling on rollback depth, and therefore on the CPU a correction can cost.
    /// Latency beyond what this covers turns into more input delay instead, up to
    /// `input_delay_max_ticks`. Zero is lockstep: no prediction at all.
    pub max_predicted_ticks: u16,
    /// How far ahead of the server the client's clock is held, in ticks, at minimum.
    ///
    /// This is the other, quieter answer to "when does the server act on my input", and the one
    /// that costs the player nothing. The client's clock already runs ahead of the server's by
    /// about half the ping, precisely so that an input stamped for tick `T` arrives before the
    /// server simulates `T`. This is the guaranteed floor under that lead, on top of what ping and
    /// jitter demand.
    ///
    /// Unlike `input_delay_min_ticks` it does **not** delay local movement: the client applies its
    /// input the moment it is pressed either way. It only moves the whole client timeline further
    /// into the future, so inputs land at the server with more slack.
    ///
    /// Lightyear's default is 1.0 — exactly one tick, the least that can work: below it the server
    /// would sometimes simulate tick `T` before the input for `T` arrived.
    pub min_client_lead_ticks: f32,
    /// TCP port for the metadata endpoint, beside the game's UDP socket.
    ///
    /// The server publishes its config there so a client can adopt the tick rate before building
    /// its app; see [`crate::metadata`]. Zero switches it off — the client then keeps whatever its
    /// own file says, and a disagreement shows up as a refused connection.
    pub meta_port: u16,
    /// How many multiples of the measured jitter to add to the client's lead.
    ///
    /// The margin covers `jitter × this`, so it is a bet on how many packets arrive in time: 1
    /// covers about 65%, 2 about 95%, 3 about 99.7%. Lightyear defaults to 4.
    pub jitter_safety_multiple: u8,
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
    /// How many ticks of player positions the server keeps to rewind into.
    ///
    /// This is the ceiling on how far a shot may reach back, so it must cover the worst connection
    /// the server means to serve: round trip plus interpolation delay. 35 ticks is about 550 ms at
    /// 64 Hz, which is generous — the memory is a few hundred bytes per player.
    ///
    /// Too short is not silent: the server logs a rewind it could not satisfy rather than quietly
    /// testing against a position the shooter never saw.
    pub lag_comp_history_ticks: u16,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            // What CS:GO's official servers run. Third-party competitive servers use 128.
            tick_hz: 64.0,
            ping_ms: 0,
            jitter_ms: 0,
            loss: 0.0,
            send_hz: 32.0,
            // Matching the tick rate, as Source does with cl_cmdrate.
            cmd_hz: 64.0,
            input_redundancy: 5,
            interp_ratio: 1.7,
            interp_min_ms: 5,
            // Lightyear's `no_input_delay()`: cover every millisecond of latency with prediction.
            input_delay_min_ticks: 0,
            input_delay_max_ticks: 0,
            max_predicted_ticks: 100,
            // Lightyear's `SyncConfig` defaults.
            min_client_lead_ticks: 1.0,
            jitter_safety_multiple: 4,
            lag_compensation: true,
            // ~550 ms at 64 Hz: past any playable connection, and cheap.
            lag_comp_history_ticks: 35,
            // Beside SERVER_PORT, which is UDP; the two do not collide.
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
        config.validate();
        config
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
            self.min_client_lead_ticks >= 1.0,
            "min_client_lead_ticks is {}, below one tick: the server would sometimes simulate a \
             tick before the input for it had arrived.",
            self.min_client_lead_ticks,
        );
        assert!(
            self.input_delay_min_ticks <= self.input_delay_max_ticks,
            "input_delay_min_ticks ({}) exceeds input_delay_max_ticks ({}): a floor above the \
             ceiling is not a setting. For a fixed delay of n ticks, set both to n.",
            self.input_delay_min_ticks,
            self.input_delay_max_ticks,
        );
        assert!(
            !self.lag_compensation || self.lag_comp_history_ticks > 0,
            "lag_compensation is on with lag_comp_history_ticks = 0: there would be no past to \
             rewind into. Set a history length, or turn lag compensation off.",
        );
    }

    fn from_file(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        toml::from_str(&text)
            .unwrap_or_else(|err| panic!("cannot parse {}: {err}", path.display()))
    }

    fn apply_env(&mut self) {
        env_parse("NOOB_TUBE_TICK_HZ", &mut self.tick_hz);
        env_parse("NOOB_TUBE_PING_MS", &mut self.ping_ms);
        env_parse("NOOB_TUBE_JITTER_MS", &mut self.jitter_ms);
        env_parse("NOOB_TUBE_LOSS", &mut self.loss);
        env_parse("NOOB_TUBE_SEND_HZ", &mut self.send_hz);
        env_parse("NOOB_TUBE_CMD_HZ", &mut self.cmd_hz);
        env_parse("NOOB_TUBE_INPUT_REDUNDANCY", &mut self.input_redundancy);
        env_parse("NOOB_TUBE_INTERP_RATIO", &mut self.interp_ratio);
        env_parse("NOOB_TUBE_INTERP_MIN_MS", &mut self.interp_min_ms);
        env_parse("NOOB_TUBE_INPUT_DELAY_MIN_TICKS", &mut self.input_delay_min_ticks);
        env_parse("NOOB_TUBE_INPUT_DELAY_MAX_TICKS", &mut self.input_delay_max_ticks);
        env_parse("NOOB_TUBE_MAX_PREDICTED_TICKS", &mut self.max_predicted_ticks);
        env_parse("NOOB_TUBE_MIN_CLIENT_LEAD_TICKS", &mut self.min_client_lead_ticks);
        env_parse("NOOB_TUBE_JITTER_SAFETY_MULTIPLE", &mut self.jitter_safety_multiple);
        env_parse("NOOB_TUBE_META_PORT", &mut self.meta_port);
        env_parse("NOOB_TUBE_LAG_COMPENSATION", &mut self.lag_compensation);
        env_parse("NOOB_TUBE_LAG_COMP_HISTORY_TICKS", &mut self.lag_comp_history_ticks);
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

    /// Adopts the parts of a server's config that the client has no business choosing.
    ///
    /// Only the tick rate. Everything else here is either the client's own preference (its
    /// simulated link, its input rate, how far in the past it draws other players) or something
    /// lightyear already learns over the wire (the server's send interval, which arrives as
    /// `SenderMetadata`). Copying the lot would mean a server dictating a client's own latency
    /// simulation, which is nonsense.
    pub fn adopt_from_server(&mut self, server: &NetConfig) {
        self.tick_hz = server.tick_hz;
    }

    /// One tick.
    pub fn tick_duration(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.tick_hz)
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
        Duration::from_secs_f64(1.0 / self.send_hz.max(f64::MIN_POSITIVE))
    }

    /// How often the client sends inputs, and how much history each packet repeats.
    ///
    /// Zero `cmd_hz` means every frame, which is lightyear's own default and what
    /// `Duration::default()` means to it.
    pub fn input_config(&self) -> input::InputConfig<crate::player::PlayerInput> {
        input::InputConfig {
            send_interval: if self.cmd_hz > 0.0 {
                Duration::from_secs_f64(1.0 / self.cmd_hz)
            } else {
                Duration::default()
            },
            packet_redundancy: self.input_redundancy,
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
            .with_send_interval_ratio(self.interp_ratio)
            .with_min_delay(Duration::from_millis(self.interp_min_ms))
    }

    /// How far ahead of the present a client stamps its inputs, and how far it may predict.
    ///
    /// Client-side only: the server acts on whatever tick an input is stamped for, and has no say
    /// in the choice.
    pub fn input_timeline(&self) -> InputTimelineConfig {
        InputTimelineConfig::default()
            .with_input_delay(client::InputDelayConfig {
                minimum_input_delay_ticks: self.input_delay_min_ticks,
                maximum_input_delay_before_prediction: self.input_delay_max_ticks,
                maximum_predicted_ticks: self.max_predicted_ticks,
            })
            .with_sync_config(SyncConfig {
                jitter_margin: self.min_client_lead_ticks,
                jitter_multiple: self.jitter_safety_multiple,
                ..SyncConfig::default()
            })
    }

    /// One line describing what is actually in effect, for the log.
    ///
    /// Printed unconditionally rather than only when something is set, because "link untouched" is
    /// the fact most worth stating: a run that was supposed to be lagged and silently was not looks
    /// exactly like a netcode success.
    pub fn describe(&self) -> String {
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
        let lag = if self.lag_compensation {
            format!("lag compensation over {} ticks", self.lag_comp_history_ticks)
        } else {
            "no lag compensation".to_string()
        };
        let stale = if std::env::var("NOOB_TUBE_LATENCY_MS").is_ok() {
            " (ignoring NOOB_TUBE_LATENCY_MS, renamed to NOOB_TUBE_PING_MS)"
        } else {
            ""
        };
        format!(
            "{link}, ticking at {:.0} Hz, sending at {:.0} Hz, inputs at {:.0} Hz ×{}, \
             interpolating at {}×, \
             input delay {}..{} ticks, predicting up to {}, client lead ≥{} ticks, \
             {lag} [{source}]{stale}",
            self.tick_hz,
            self.send_hz,
            self.cmd_hz,
            self.input_redundancy,
            self.interp_ratio,
            self.input_delay_min_ticks,
            self.input_delay_max_ticks,
            self.max_predicted_ticks,
            self.min_client_lead_ticks,
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
        let config = NetConfig { send_hz: 32.0, ..default_config() };
        assert_eq!(config.send_interval(), Duration::from_secs_f64(1.0 / 32.0));
    }

    /// A file may set any subset; the rest stays at its default.
    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let config: NetConfig = toml::from_str("ping_ms = 120").unwrap();
        assert_eq!(config.ping_ms, 120);
        assert_eq!(config.send_hz, NetConfig::default().send_hz);
    }

    /// The whole point of `deny_unknown_fields`: a typo must not read as "no latency".
    #[test]
    fn a_misspelled_key_is_refused() {
        assert!(toml::from_str::<NetConfig>("pign_ms = 120").is_err());
    }

    #[test]
    fn no_input_delay_by_default() {
        let config = default_config();
        assert_eq!(config.input_delay_min_ticks, 0);
        assert_eq!(config.input_delay_max_ticks, 0);
    }

    #[test]
    fn a_fixed_delay_sets_both_ends() {
        let config = NetConfig {
            input_delay_min_ticks: 3,
            input_delay_max_ticks: 3,
            ..default_config()
        };
        config.validate();
        // `InputTimelineConfig` keeps its fields private, so this asserts that the pair we hand
        // lightyear is the one that means "always three ticks, on any link".
        assert_eq!(config.input_delay_min_ticks, config.input_delay_max_ticks);
        let _ = config.input_timeline();
    }

    /// A floor above the ceiling would otherwise reach lightyear and assert from inside a system.
    #[test]
    #[should_panic(expected = "input_delay_min_ticks")]
    fn a_floor_above_the_ceiling_is_refused() {
        NetConfig {
            input_delay_min_ticks: 5,
            input_delay_max_ticks: 2,
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
            lag_comp_history_ticks: 0,
            ..default_config()
        }
        .validate();
    }

    /// Turning it off is allowed to zero the history — there is then nothing to keep.
    #[test]
    fn no_lag_compensation_needs_no_history() {
        NetConfig {
            lag_compensation: false,
            lag_comp_history_ticks: 0,
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
        let lead = NetConfig { min_client_lead_ticks: 6.0, ..default_config() };
        lead.validate();
        assert_eq!(lead.input_delay_min_ticks, 0, "raising the lead must not delay local input");
    }

    #[test]
    #[should_panic(expected = "min_client_lead_ticks")]
    fn a_lead_below_one_tick_is_refused() {
        NetConfig { min_client_lead_ticks: 0.5, ..default_config() }.validate();
    }

    /// The whole point of the metadata endpoint, and its whole limit: the tick rate crosses, the
    /// client's own preferences do not.
    #[test]
    fn only_the_tick_rate_is_adopted() {
        let server = NetConfig {
            tick_hz: 128.0,
            ping_ms: 300,
            cmd_hz: 20.0,
            interp_ratio: 4.0,
            ..default_config()
        };
        let mut client = NetConfig { tick_hz: 64.0, ping_ms: 50, ..default_config() };

        client.adopt_from_server(&server);

        assert_eq!(client.tick_hz, 128.0, "the tick rate has to cross");
        assert_eq!(client.ping_ms, 50, "the server does not choose our link simulation");
        assert_eq!(client.cmd_hz, default_config().cmd_hz);
        assert_eq!(client.interp_ratio, default_config().interp_ratio);
    }

    /// The endpoint speaks the same TOML the config file does, so a round trip must be lossless.
    #[test]
    fn the_config_survives_a_round_trip() {
        let original = NetConfig { tick_hz: 128.0, ping_ms: 70, cmd_hz: 20.0, ..default_config() };
        let text = toml::to_string(&original).unwrap();
        assert_eq!(toml::from_str::<NetConfig>(&text).unwrap(), original);
    }

    #[test]
    fn a_command_rate_becomes_an_interval() {
        let config = NetConfig { cmd_hz: 64.0, ..default_config() };
        assert_eq!(
            config.input_config().send_interval,
            Duration::from_secs_f64(1.0 / 64.0)
        );
    }

    /// Zero is lightyear's "every frame", which `Duration::default()` is how it spells.
    #[test]
    fn a_command_rate_of_zero_means_every_frame() {
        let config = NetConfig { cmd_hz: 0.0, ..default_config() };
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
        let b = NetConfig { ping_ms: 250, send_hz: 20.0, ..default_config() };
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
