//! Code shared by the client and the server.
//!
//! Everything in here must run on the headless server as well as in the rendering client, so it
//! must not depend on rendering, windowing or asset loading.

use core::time::Duration;

pub mod collision;
pub mod level;
pub mod movement;
pub mod player;
pub mod protocol;
pub mod types;
#[cfg(feature = "remote")]
pub mod remote;

/// Server simulation rate. Client prediction replays at the same rate.
pub const TICK_RATE: f64 = 64.0;

pub fn tick_duration() -> Duration {
    Duration::from_secs_f64(1.0 / TICK_RATE)
}

/// Default UDP port the server listens on.
pub const SERVER_PORT: u16 = 5000;

/// Netcode protocol id. Clients and servers only talk to each other when these match, so bumping
/// it locks out incompatible builds.
pub const PROTOCOL_ID: u64 = 0x_4E_4F_4F_42_54_55_42_45; // "NOOBTUBE"

/// Netcode private key.
///
/// This placeholder is fine while client and server are started by hand on one machine. A public
/// server needs a real key handed out by a backend that issues connect tokens — see the netcode
/// standard for what that involves.
pub const PLACEHOLDER_PRIVATE_KEY: [u8; 32] = [0; 32];

#[cfg(test)]
mod tests {
    /// rapier's math types must be Bevy's, or every call into the collision code would need
    /// converting. This holds only while rapier is the `-glamx0.2` build; it breaks loudly if
    /// someone bumps to a version compiled against a different glam.
    #[test]
    fn rapier_and_bevy_share_glam() {
        let v: rapier3d::math::Vector = bevy::math::Vec3::new(1.0, 2.0, 3.0);
        assert_eq!(v.x, 1.0);
    }
}
