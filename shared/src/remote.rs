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

use crate::player::{PlayerInput, PlayerState};

/// BRP port for the client. This is the protocol's default, so external tools find it unprompted,
/// and it is also what `BrpExtrasPlugin` binds when the client leaves the transport to it.
pub const CLIENT_REMOTE_PORT: u16 = 15702;

/// BRP port for the server, clear of the client's, so both can run on one machine.
pub const SERVER_REMOTE_PORT: u16 = 15712;

/// Puts the shared types into the registry, without opening a port.
///
/// Split out from [`RemoteInspectPlugin`] because the transport can come from elsewhere: the client
/// lets `BrpExtrasPlugin` own it, and two plugins adding `RemoteHttpPlugin` is a warning at best.
pub struct RemoteTypesPlugin;

impl Plugin for RemoteTypesPlugin {
    fn build(&self, app: &mut App) {
        app
            // A headless server built on MinimalPlugins has almost nothing in its type registry —
            // not even `Name`, which is what makes a listing of entities readable at all.
            .register_type::<Name>()
            // Field types have to be in the registry too, or a component that reflects fine still
            // fails to serialise.
            .register_type::<Vec2>()
            .register_type::<Vec3>()
            .register_type::<PlayerInput>()
            .register_type::<PlayerState>();
    }
}

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
            .add_plugins(RemoteTypesPlugin);
    }
}
