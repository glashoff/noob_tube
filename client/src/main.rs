//! Game client: renders the world, samples input, predicts the local player.

mod debug_draw;
mod harness;
mod local_player;
mod remote_players;
mod world;

use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::{PLACEHOLDER_PRIVATE_KEY, PROTOCOL_ID, SERVER_PORT, tick_duration};
use std::net::{Ipv4Addr, SocketAddr};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "Noob Tube".into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(Time::<Fixed>::from_hz(noob_tube_shared::TICK_RATE))
        // How far in the past other players are drawn. Inserted before the plugin group, which
        // only fills this in if it is missing.
        .insert_resource(noob_tube_shared::tuning::interpolation())
        .add_plugins((
            world::WorldPlugin,
            noob_tube_shared::types::SharedTypesPlugin,
            local_player::LocalPlayerPlugin,
            debug_draw::DebugDrawPlugin,
            remote_players::RemotePlayersPlugin,
            harness::HarnessPlugin,
        ))
        .add_plugins(client::ClientPlugins {
            tick_duration: tick_duration(),
        })
        // After the plugin group, before the Client entity is spawned.
        .add_plugins(noob_tube_shared::protocol::ProtocolPlugin)
        .add_systems(Startup, connect)
        .add_observer(on_connected)
        .add_plugins(remote_inspection())
        .add_plugins(world_inspector())
        .run();
}

/// Live ECS inspection over BRP, compiled in only with `--features remote`.
///
/// `BrpExtrasPlugin` owns the transport here rather than `RemoteInspectPlugin`. It adds
/// `RemotePlugin` and the HTTP server itself — on `CLIENT_REMOTE_PORT`, which is its default too —
/// and extends BRP with methods for screenshots and synthetic keyboard and mouse input. Those are
/// what let an agent drive the running game the way `harness.rs` does, but from outside and without
/// a scripted path compiled in.
#[cfg(feature = "remote")]
fn remote_inspection() -> impl Plugin {
    |app: &mut App| {
        app.add_plugins(bevy_brp_extras::BrpExtrasPlugin::new());
    }
}

/// Without the feature there is nothing to add, and `()` is a valid empty plugin group.
#[cfg(not(feature = "remote"))]
fn remote_inspection() -> impl Plugin {
    |_: &mut App| {}
}

/// Two egui inspector windows, compiled in with `--features inspector`.
///
/// Unlike BRP these run inside the process, so they cannot see the server and they draw over the
/// game. Both start hidden — while one is up, egui takes the pointer, which fights the locked cursor
/// that mouse look needs.
///
/// **F1 is the world**, shown as a tree of roots that expand into their children. It is readable
/// because the level has a hierarchy: `Level` holds the ground, the props and the sun, so the top
/// level is half a dozen entries rather than everything at once. It already hides observers and the
/// entities Bevy 0.19 uses to store resources.
///
/// **F2 filters on `With<Name>`** — a flat list of what this crate spawns, since that is what we
/// bother to name. Useful for reaching one known entity without expanding anything, but it ignores
/// the hierarchy and so shows parents beside their own children.
///
/// Both show only what derives `Reflect` and is registered, the same requirement BRP has.
#[cfg(feature = "inspector")]
fn world_inspector() -> impl Plugin {
    use bevy::input::common_conditions::input_toggle_active;
    use bevy_inspector_egui::bevy_egui::EguiPlugin;
    use bevy_inspector_egui::quick::{FilterQueryInspectorPlugin, WorldInspectorPlugin};

    |app: &mut App| {
        // The inspectors warn rather than add this themselves, so it has to come first.
        app.add_plugins(EguiPlugin::default())
            .add_plugins(
                WorldInspectorPlugin::new().run_if(input_toggle_active(false, KeyCode::F1)),
            )
            .add_plugins(
                FilterQueryInspectorPlugin::<With<noob_tube_shared::types::Authored>>::default()
                    .run_if(input_toggle_active(false, KeyCode::F2)),
            );
    }
}

#[cfg(not(feature = "inspector"))]
fn world_inspector() -> impl Plugin {
    |_: &mut App| {}
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
            noob_tube_shared::types::Authored,
            Client,
            ReplicationReceiver,
            Link::default().with_conditioner(noob_tube_shared::tuning::conditioner()),
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
    info!("connecting to {server_addr}, {}", noob_tube_shared::tuning::describe());
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
