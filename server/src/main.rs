//! Headless authoritative server.
//!
//! Simulates the game at `TICK_RATE` and replicates state to connected clients. It runs no
//! renderer, no window and no audio — see this crate's `Cargo.toml`, where Bevy's default features
//! are switched off.

use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::{PLACEHOLDER_PRIVATE_KEY, PROTOCOL_ID, SERVER_PORT, tick_duration};
use std::net::{Ipv4Addr, SocketAddr};

fn main() {
    App::new()
        .add_plugins(MinimalPlugins)
        // lightyear registers states; MinimalPlugins does not include StatesPlugin.
        .add_plugins(bevy::state::app::StatesPlugin)
        .add_plugins(bevy::log::LogPlugin::default())
        .add_plugins(server::ServerPlugins {
            tick_duration: tick_duration(),
        })
        .add_systems(Startup, start_listening)
        .add_observer(on_client_connected)
        .run();
}

/// Binds the UDP socket and starts accepting connections.
fn start_listening(mut commands: Commands) {
    let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), SERVER_PORT);

    let server = commands
        .spawn((
            Name::from("Server"),
            server::NetcodeServer::new(server::NetcodeConfig {
                protocol_id: PROTOCOL_ID,
                private_key: PLACEHOLDER_PRIVATE_KEY,
                ..default()
            }),
            server::ServerUdpIo::default(),
            LocalAddr(addr),
        ))
        .id();

    commands.trigger(server::Start { entity: server });
    info!("listening on {addr}");
}

/// Fires once per incoming connection. Lightyear spawns a child entity carrying `LinkOf` for each
/// client; this is where per-connection components get attached.
fn on_client_connected(trigger: On<Add, LinkOf>, mut commands: Commands) {
    let entity = trigger.entity;
    // ReplicationSender is what lets us replicate local entities to this client.
    commands
        .entity(entity)
        .insert((ReplicationSender, Name::from("Connection")));
    info!("client connected: {entity}");
}
