//! Simulated network conditions, so netcode can be judged at all.
//!
//! On localhost there is no latency, no jitter and no packet loss, which means every netcode
//! behaviour worth having — prediction, reconciliation, interpolation — is invisible and every
//! measurement is meaningless. The 0.000000 agreement between the client's simulation and the
//! server's says only that two identical computations with no disturbance produce the same answer.
//!
//! Configured through the environment rather than in code, so switching between conditions costs a
//! restart rather than a rebuild:
//!
//! ```text
//! NOOB_TUBE_LATENCY_MS=30     one-way delay, so RTT is twice this
//! NOOB_TUBE_JITTER_MS=5       random variation around it, ± this
//! NOOB_TUBE_LOSS=0.02         packet loss probability, 0.0 to 1.0
//! ```
//!
//! The conditioner delays *incoming* payloads only. Setting the same values on both ends therefore
//! produces a symmetric link with a round trip of twice the latency.

use core::time::Duration;

use lightyear::prelude::*;

/// Reads the conditioner settings from the environment.
///
/// Returns `None` when no latency, jitter or loss is asked for, which leaves the link untouched.
pub fn from_env() -> Option<RecvLinkConditioner> {
    let latency = env_ms("NOOB_TUBE_LATENCY_MS");
    let jitter = env_ms("NOOB_TUBE_JITTER_MS");
    let loss = std::env::var("NOOB_TUBE_LOSS")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .unwrap_or(0.0);

    if latency.is_zero() && jitter.is_zero() && loss <= 0.0 {
        return None;
    }

    let config = LinkConditionerConfig::default()
        .with_incoming_latency(latency)
        .with_incoming_jitter(jitter)
        .with_fixed_loss(loss.clamp(0.0, 1.0));
    Some(RecvLinkConditioner::new(config))
}

/// Describes the current settings for a log line, or `None` if the link is untouched.
pub fn describe() -> Option<String> {
    from_env().map(|_| {
        format!(
            "latency {:?} one-way, jitter {:?}, loss {}",
            env_ms("NOOB_TUBE_LATENCY_MS"),
            env_ms("NOOB_TUBE_JITTER_MS"),
            std::env::var("NOOB_TUBE_LOSS").unwrap_or_else(|_| "0".into()),
        )
    })
}

fn env_ms(name: &str) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_default()
}
