//! Game client: renders the world, samples input, predicts the local player.

use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::{PLACEHOLDER_PRIVATE_KEY, PROTOCOL_ID, SERVER_PORT, tick_duration};
use std::net::{Ipv4Addr, SocketAddr};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(client::ClientPlugins {
            tick_duration: tick_duration(),
        })
        .add_systems(Startup, connect)
        .add_observer(on_connected)
        .run();
}

fn connect(mut commands: Commands) {
    let server_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), SERVER_PORT);
    // Port 0 lets the OS pick, so several clients can run on one machine.
    let local_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0);

    let auth = Authentication::Manual {
        server_addr,
        client_id: rand_client_id(),
        private_key: PLACEHOLDER_PRIVATE_KEY,
        protocol_id: PROTOCOL_ID,
    };

    // Prediction needs its manager resource in place before the client entity exists.
    commands.insert_resource(PredictionManager::default());

    let client = commands
        .spawn((
            Name::from("Client"),
            Client,
            ReplicationReceiver,
            Link::default(),
            // Only server-side `ClientOf` entities get a PingManager registered automatically,
            // so the client adds its own. Without it the server's pings arrive but nothing
            // answers them, and the timelines never synchronise.
            PingManager::default(),
            client::NetcodeClient::new(
                auth,
                client::NetcodeConfig {
                    // Never expire: this token is minted locally, there is no backend to reissue it.
                    token_expire_secs: -1,
                    ..default()
                },
            )
            .expect("failed to build the netcode client"),
            UdpIo::default(),
            LocalAddr(local_addr),
            PeerAddr(server_addr),
        ))
        .id();

    commands.trigger(client::Connect { entity: client });
    info!("connecting to {server_addr}");
}

fn on_connected(trigger: On<Add, Connected>) {
    info!("connected: {}", trigger.entity);
}

/// Distinct id per process so two clients on one machine do not collide.
fn rand_client_id() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
