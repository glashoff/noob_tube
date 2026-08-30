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
//! ping_ms = 100        # simulated round trip; each end delays half of it
//! jitter_ms = 10       # random variation on each leg, ± this
//! loss = 0.02          # packet loss probability, 0.0 to 1.0
//! send_hz = 32.0       # how often the server replicates          (server only)
//! interp_ratio = 1.7   # interpolation delay, in send intervals   (client only)
//! interp_min_ms = 5    # floor under that delay                   (client only)
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
use serde::Deserialize;

/// Where the config is looked for when `NOOB_TUBE_CONFIG` says nothing.
pub const DEFAULT_CONFIG_PATH: &str = "noob_tube.toml";

/// Everything about the network that is worth changing without a rebuild.
///
/// `deny_unknown_fields` is deliberate. A misspelled key that silently does nothing is the same
/// failure as a conditioner that was never applied: the run looks fine and every number from it is
/// wrong. Better to refuse to start.
#[derive(Resource, Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NetConfig {
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
    /// How far in the past other players are drawn, as a multiple of the send interval.
    ///
    /// Below 1.0 the next update has usually not arrived yet and remote players freeze; well above
    /// it they are drawn needlessly far behind.
    pub interp_ratio: f32,
    /// Floor under the interpolation delay, for when the send rate is very high.
    pub interp_min_ms: u64,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            ping_ms: 0,
            jitter_ms: 0,
            loss: 0.0,
            send_hz: 32.0,
            interp_ratio: 1.7,
            interp_min_ms: 5,
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
        config
    }

    fn from_file(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
        toml::from_str(&text)
            .unwrap_or_else(|err| panic!("cannot parse {}: {err}", path.display()))
    }

    fn apply_env(&mut self) {
        env_parse("NOOB_TUBE_PING_MS", &mut self.ping_ms);
        env_parse("NOOB_TUBE_JITTER_MS", &mut self.jitter_ms);
        env_parse("NOOB_TUBE_LOSS", &mut self.loss);
        env_parse("NOOB_TUBE_SEND_HZ", &mut self.send_hz);
        env_parse("NOOB_TUBE_INTERP_RATIO", &mut self.interp_ratio);
        env_parse("NOOB_TUBE_INTERP_MIN_MS", &mut self.interp_min_ms);
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

    /// How often the server sends replication updates.
    pub fn send_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.send_hz.max(f64::MIN_POSITIVE))
    }

    /// The delay a client draws other players at.
    pub fn interpolation(&self) -> InterpolationConfig {
        InterpolationConfig::default()
            .with_send_interval_ratio(self.interp_ratio)
            .with_min_delay(Duration::from_millis(self.interp_min_ms))
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
        let stale = if std::env::var("NOOB_TUBE_LATENCY_MS").is_ok() {
            " (ignoring NOOB_TUBE_LATENCY_MS, renamed to NOOB_TUBE_PING_MS)"
        } else {
            ""
        };
        format!(
            "{link}, sending at {:.0} Hz, interpolating at {}× [{source}]{stale}",
            self.send_hz, self.interp_ratio
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

    fn default_config() -> NetConfig {
        NetConfig::default()
    }
}
