//! Code shared by the client and the server.
//!
//! Everything in here must run on the headless server as well as in the rendering client, so it
//! must not depend on rendering, windowing or asset loading.


pub mod hitbox;
pub mod lag_compensation;
pub mod level;
pub mod metadata;
pub mod movement;
pub mod physics;
pub mod player;
pub mod props;
pub mod protocol;
pub mod shooting;
pub mod simulation;
pub mod tuning;
pub mod types;
#[cfg(feature = "remote")]
pub mod remote;


/// Default UDP port the server listens on.
pub const SERVER_PORT: u16 = 5000;

/// Base netcode protocol id. Clients and servers only talk to each other when these match, so
/// bumping it locks out incompatible builds.
///
/// Not used directly: [`NetConfig::protocol_id`](tuning::NetConfig::protocol_id) mixes the tick
/// rate in, so two peers that disagree about it cannot connect at all.
pub const PROTOCOL_ID: u64 = 0x_4E_4F_4F_42_54_55_42_45; // "NOOBTUBE"

/// Netcode private key.
///
/// This placeholder is fine while client and server are started by hand on one machine. A public
/// server needs a real key handed out by a backend that issues connect tokens — see the netcode
/// standard for what that involves.
pub const PLACEHOLDER_PRIVATE_KEY: [u8; 32] = [0; 32];
