//! Every network knob, in one place, read from the environment.
//!
//! These are settings, not constants: what a shooter feels like at 30 ms and at 150 ms are
//! different games, and finding out which trade is right means changing a number and playing, not
//! changing a number and waiting for a link step. So they live in the environment and cost a
//! restart, and every process that needs one reads it from here.
//!
//! ```text
//! NOOB_TUBE_PING_MS=100      simulated round trip; both ends delay half of it
//! NOOB_TUBE_JITTER_MS=10     random variation on each leg, ± this
//! NOOB_TUBE_LOSS=0.02        packet loss probability, 0.0 to 1.0
//! NOOB_TUBE_SEND_HZ=32       how often the server replicates (server only)
//! NOOB_TUBE_INTERP_RATIO=1.7 interpolation delay, in send intervals (client only)
//! NOOB_TUBE_INTERP_MIN_MS=5  floor under that delay (client only)
//! ```
//!
//! The conditioner has to be set on **every process**, because it delays only what a process
//! *receives*: the server's copy delays inputs on their way in, each client's copy delays snapshots
//! on their way in. Set it on the server alone and you get a half-duplex link that behaves like
//! nothing real.

use core::time::Duration;

use lightyear::prelude::*;

/// Default replication rate in Hz, deliberately below the tick rate.
///
/// Lightyear's own default is zero, meaning an update every frame. That is more bandwidth than a
/// shooter needs, and it hides the problem interpolation exists to solve: with no gap between
/// updates there is nothing to interpolate across, so remote players look smooth for the wrong
/// reason and start stepping the moment the rate drops.
pub const DEFAULT_SEND_HZ: f64 = 32.0;

/// Default interpolation delay, as a multiple of the send interval.
///
/// Lightyear's default. Below 1.0 the next update has usually not arrived yet and remote players
/// freeze; well above it they are drawn needlessly far in the past.
pub const DEFAULT_INTERP_RATIO: f32 = 1.7;

/// Default floor under the interpolation delay.
pub const DEFAULT_INTERP_MIN_MS: u64 = 5;

/// Simulated round trip. Zero means an untouched link.
pub fn ping() -> Duration {
    env_ms("NOOB_TUBE_PING_MS").unwrap_or_default()
}

/// Random variation added to each leg.
pub fn jitter() -> Duration {
    env_ms("NOOB_TUBE_JITTER_MS").unwrap_or_default()
}

/// Packet loss probability, 0.0 to 1.0.
pub fn loss() -> f32 {
    std::env::var("NOOB_TUBE_LOSS")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0)
}

/// How often the server sends replication updates.
pub fn send_interval() -> Duration {
    let hz = std::env::var("NOOB_TUBE_SEND_HZ")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|hz| *hz > 0.0)
        .unwrap_or(DEFAULT_SEND_HZ);
    Duration::from_secs_f64(1.0 / hz)
}

/// How far in the past a client draws other players.
///
/// Derived from the *server's* send rate, which the client learns over the wire, so this is only
/// the multiplier and the floor.
pub fn interpolation() -> InterpolationConfig {
    let ratio = std::env::var("NOOB_TUBE_INTERP_RATIO")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(DEFAULT_INTERP_RATIO);
    let min_delay = env_ms("NOOB_TUBE_INTERP_MIN_MS")
        .unwrap_or(Duration::from_millis(DEFAULT_INTERP_MIN_MS));
    InterpolationConfig::default()
        .with_send_interval_ratio(ratio)
        .with_min_delay(min_delay)
}

/// The link conditioner for this process, or `None` when nothing is being simulated.
///
/// Each end delays only what it receives, so it gets **half** the ping: the two halves add up to
/// the round trip the caller asked for. Getting this wrong in either direction is the easiest
/// mistake here — a "100 ms ping" that turns out to be 200 makes every later number wrong.
pub fn conditioner() -> Option<RecvLinkConditioner> {
    let (ping, jitter, loss) = (ping(), jitter(), loss());
    if ping.is_zero() && jitter.is_zero() && loss <= 0.0 {
        return None;
    }
    let config = LinkConditionerConfig::default()
        .with_incoming_latency(ping / 2)
        .with_incoming_jitter(jitter)
        .with_fixed_loss(loss);
    Some(RecvLinkConditioner::new(config))
}

/// One line describing what is actually in effect, for the log.
///
/// Printed unconditionally rather than only when something is set, because "no conditioner" is the
/// fact most worth stating: a run that was supposed to be lagged and silently was not looks like a
/// netcode success.
pub fn describe() -> String {
    let mut line = if ping().is_zero() && jitter().is_zero() && loss() <= 0.0 {
        "link untouched".to_string()
    } else {
        format!(
            "ping {} ms, jitter ±{} ms per leg, loss {}",
            ping().as_millis(),
            jitter().as_millis(),
            loss(),
        )
    };
    if std::env::var("NOOB_TUBE_LATENCY_MS").is_ok() {
        line.push_str(" (ignoring NOOB_TUBE_LATENCY_MS, renamed to NOOB_TUBE_PING_MS)");
    }
    line
}

fn env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
}
