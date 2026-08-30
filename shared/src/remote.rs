//! Live inspection of the running ECS over the Bevy Remote Protocol.
//!
//! BRP is a JSON-RPC endpoint that lets an external tool query and modify the world of a running
//! app. Two properties make it the right choice here:
//!
//! - It needs no window, so the headless server can be inspected the same way as the client.
//! - Its `+watch` methods stream *changes* rather than snapshots, which is how we record what the
//!   ECS does over time instead of polling and diffing by hand.
//!
//! Everything BRP can see goes through reflection, so a type is invisible unless it derives
//! `Reflect` and is registered. That is what [`RemoteInspectPlugin`] does for the shared types.

use bevy::prelude::*;
use bevy_remote::RemotePlugin;
use bevy_remote::http::RemoteHttpPlugin;

use crate::types::SharedTypesPlugin;

/// BRP port for the client. This is the protocol's default, so external tools find it unprompted,
/// and it is also what `BrpExtrasPlugin` binds when the client leaves the transport to it.
pub const CLIENT_REMOTE_PORT: u16 = 15702;

/// BRP port for the server, clear of the client's, so both can run on one machine.
pub const SERVER_REMOTE_PORT: u16 = 15712;

/// Serves BRP on `port`, bound to localhost only, and registers the shared types.
pub struct RemoteInspectPlugin {
    pub port: u16,
}

impl Plugin for RemoteInspectPlugin {
    fn build(&self, app: &mut App) {
        app
            // RemotePlugin holds the method registry and processes requests; the transport is a
            // separate plugin, so BRP could later be served over something other than HTTP.
            .add_plugins(RemotePlugin::default())
            .add_plugins(RemoteHttpPlugin::default().with_port(self.port))
            .add_plugins(SharedTypesPlugin);
    }
}
