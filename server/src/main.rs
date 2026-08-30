//! Headless authoritative server.
//!
//! Simulates the game at `TICK_RATE` and replicates state to connected clients. It runs no
//! renderer, no window and no audio — see this crate's `Cargo.toml`, where Bevy's default features
//! are switched off.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::input::native::ActionState;
use noob_tube_shared::level;
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState};
use noob_tube_shared::protocol::ProtocolPlugin;
use noob_tube_shared::types::Authored;
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
        // The protocol must be registered after the plugin group and before any Server entity.
        .add_plugins(ProtocolPlugin)
        .insert_resource(Time::<Fixed>::from_hz(noob_tube_shared::TICK_RATE))
        // The same geometry the client collides against, built from the same numbers. If the two
        // disagreed, every step near the difference would produce a correction the player sees.
        .insert_resource(level::collision_world())
        .add_systems(Startup, start_listening)
        .add_systems(FixedUpdate, step_players)
        .add_observer(on_client_connected)
        .add_observer(on_peer_connected)
        .add_plugins(remote_inspection())
        .run();
}

/// Live ECS inspection over BRP, compiled in only with `--features remote`.
///
/// The server has no window, so this is the only way to look inside it while it runs.
#[cfg(feature = "remote")]
fn remote_inspection() -> impl Plugin {
    noob_tube_shared::remote::RemoteInspectPlugin {
        port: noob_tube_shared::remote::SERVER_REMOTE_PORT,
    }
}

/// Without the feature there is nothing to add, and `()` is a valid empty plugin group.
#[cfg(not(feature = "remote"))]
fn remote_inspection() -> impl Plugin {
    |_: &mut App| {}
}

/// Binds the UDP socket and starts accepting connections.
fn start_listening(mut commands: Commands) {
    let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), SERVER_PORT);

    let server = commands
        .spawn((
            Name::from("Server"),
            Authored,
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
        .insert((ReplicationSender, Name::from("Connection"), Authored));
    info!("client connected: {entity}");
}

/// Fires once the handshake finishes and the peer has an identity.
///
/// The player entity is spawned here rather than on `LinkOf`, because only now is there a `PeerId`
/// to put in [`Player`] — and without it a client cannot tell its own player from anyone else's.
fn on_peer_connected(
    trigger: On<Add, Connected>,
    peers: Query<&RemoteId>,
    players: Query<&Player>,
    mut commands: Commands,
) {
    let Ok(remote) = peers.get(trigger.entity) else {
        return;
    };
    let PeerId::Netcode(peer) = remote.0 else {
        return;
    };

    let mut state = PlayerState::default();
    state.position = level::spawn_point(players.iter().count());

    commands.spawn((
        Name::from(format!("Player {peer}")),
        Authored,
        Player { peer },
        state,
        Aim::default(),
        // Where this client's inputs are written once they arrive.
        ActionState::<PlayerInput>::default(),
        // Replicate is the other half of ReplicationSender: that says the channel may send, this
        // says the entity should be sent.
        Replicate::to_clients(NetworkTarget::All),
        // Says which connection owns this player. It arrives at that one client as `Controlled`,
        // which is how a client recognises its own player without comparing peer ids — and it is
        // what routes that client's inputs to this entity.
        ControlledBy {
            owner: trigger.entity,
            lifetime: Lifetime::SessionBased,
        },
    ));
    info!("player spawned for peer {peer}");
}

/// FixedUpdate: advances every player by one tick from the input that arrived for it.
///
/// This is the authoritative simulation. It calls the same `apply_input` the client runs locally,
/// against collision geometry built from the same constants — that identity is what makes client
/// prediction possible at all in M4. Anything read here that the client cannot also read would show
/// up as a correction the player feels.
fn step_players(
    world: Res<noob_tube_shared::collision::CollisionWorld>,
    time: Res<Time<Fixed>>,
    mut players: Query<(&ActionState<PlayerInput>, &mut PlayerState, &mut Aim)>,
) {
    let dt = time.delta_secs();
    for (action, mut state, mut aim) in players.iter_mut() {
        let input = action.0;
        state.apply_input(&input, &world, dt);
        // Aim is not simulated; it is simply what the client reported, forwarded so other clients
        // can draw where this player looks.
        aim.yaw = input.yaw;
        aim.pitch = input.pitch;
    }
}
