//! Headless authoritative server.
//!
//! Simulates the game at the configured tick rate and replicates state to connected clients. It runs no
//! renderer, no window and no audio — see this crate's `Cargo.toml`, where Bevy's default features
//! are switched off.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::input::native::ActionState;
use noob_tube_shared::simulation;
use noob_tube_shared::tuning::NetConfig;
use noob_tube_shared::level;
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState};
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::shooting::{self, Health};
use noob_tube_shared::protocol::ProtocolPlugin;
use noob_tube_shared::types::Authored;
use noob_tube_shared::{PLACEHOLDER_PRIVATE_KEY, SERVER_PORT};
use std::net::{Ipv4Addr, SocketAddr};

fn main() {
    // Read once, at startup, before anything can ask for it.
    let net = NetConfig::load();


    App::new()
        .add_plugins(MinimalPlugins)
        // lightyear registers states; MinimalPlugins does not include StatesPlugin.
        .add_plugins(bevy::state::app::StatesPlugin)
        .add_plugins(bevy::log::LogPlugin::default())
        .add_plugins(server::ServerPlugins {
            tick_duration: net.tick_duration(),
        })
        // The protocol must be registered after the plugin group and before any Server entity.
        .add_plugins(ProtocolPlugin { net })
        .insert_resource(Time::<Fixed>::from_hz(net.tick_hz))
        // How often replication updates go out. Without this lightyear sends every frame, and
        // interpolation then has nothing to interpolate across — see `SEND_RATE`.
        .insert_resource(ReplicationMetadata::new(net.send_interval()))
        .insert_resource(net)
        // The same geometry the client collides against, built from the same numbers. If the two
        // disagreed, every step near the difference would produce a correction the player sees.
        .insert_resource(level::collision_world())
        .add_systems(Startup, (start_listening, publish_metadata))
        .add_systems(
            FixedUpdate,
            // Before the step, which consumes the trigger by starting the cooldown, and which
            // moves everyone. A shot has to be resolved against the positions its shooter was
            // looking at, not the ones a tick of movement later.
            (resolve_shots, simulation::step_players::<()>).chain(),
        )
        .add_observer(on_client_connected)
        .add_observer(on_peer_connected)
        .add_plugins(remote_inspection())
        .run();
}

/// Which spawn point a player returns to. Server-side, never replicated.
#[derive(Component, Clone, Copy)]
struct SpawnIndex(usize);

/// FixedUpdate: fires every trigger that is down and ready, and applies the damage.
///
/// Server only. The client predicts its own cooldown, so the weapon answers the trigger without
/// waiting for a round trip, but whether anyone was *hit* is decided here and only here.
///
/// Two passes, because a shooter cannot hold everyone else's health mutably while looking for a
/// target. The first reads; the second writes.
fn resolve_shots(
    world: Res<CollisionWorld>,
    // A ParamSet because the two halves both want `PlayerState` — the first to aim at, the second
    // to respawn. Bevy refuses two queries with overlapping mutable access held at once, and it is
    // right to: the reads below finish before any write starts, but nothing in the signature says
    // so. The set makes that ordering explicit instead of asserted.
    mut players: ParamSet<(
        Query<(Entity, &PlayerState, &Aim, &ActionState<PlayerInput>, &Player)>,
        Query<(&mut Health, &mut PlayerState, &Player, &SpawnIndex)>,
    )>,
) {
    // Everyone who could be hit, gathered once rather than once per shooter.
    let targets: Vec<(Entity, Vec3, bool)> = players
        .p0()
        .iter()
        .map(|(entity, state, ..)| (entity, state.position, state.crouching))
        .collect();

    let mut hits: Vec<(Entity, u64)> = Vec::new();
    for (shooter, state, aim, action, player) in players.p0().iter() {
        if !state.is_firing(&action.0) {
            continue;
        }
        let (origin, direction) = shooting::aim_ray(state.eye_position(), aim.yaw, aim.pitch);
        // Everyone but the shooter. Left in, they would hit themselves at zero distance.
        let others = targets.iter().copied().filter(|(entity, ..)| *entity != shooter);
        if let Some((hit, distance)) = shooting::resolve(&world, origin, direction, others) {
            debug!("peer {} hit at {distance:.1} m", player.peer);
            hits.push((hit, player.peer));
        }
    }

    for (target, shooter) in hits {
        let mut wounded = players.p1();
        let Ok((mut health, mut state, player, spawn)) = wounded.get_mut(target) else {
            continue;
        };
        if !health.hurt(shooting::WEAPON_DAMAGE) {
            continue;
        }
        info!("peer {shooter} killed peer {}", player.peer);
        // Respawning by overwriting the state is the whole of death for now: no ragdoll, no
        // delay, no score. The killed client's prediction disagrees for one round trip and then
        // rolls back to here, which is exactly the correction rollback exists for.
        *health = Health::default();
        *state = PlayerState {
            position: level::spawn_point(spawn.0),
            ..PlayerState::default()
        };
    }
}

/// FixedUpdate: advances every player by one tick, compiled in only with `--features remote`.
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
fn start_listening(net: Res<NetConfig>, mut commands: Commands) {
    let addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), SERVER_PORT);

    let server = commands
        .spawn((
            Name::from("Server"),
            Authored,
            server::NetcodeServer::new(server::NetcodeConfig {
                protocol_id: net.protocol_id(),
                private_key: PLACEHOLDER_PRIVATE_KEY,
                ..default()
            }),
            server::ServerUdpIo::default(),
            LocalAddr(addr),
        ))
        .id();

    commands.trigger(server::Start { entity: server });
    info!("listening on {addr}");
    info!("{}", net.describe());
}

/// Startup: publishes the config beside the game socket, so a client can adopt the tick rate
/// before it builds its app.
///
/// A Startup system rather than a plain call from `main`, only so that its log line has somewhere
/// to go: the tracing subscriber arrives with `LogPlugin`, inside the App. Zero switches it off.
fn publish_metadata(net: Res<NetConfig>) {
    if net.meta_port != 0 {
        noob_tube_shared::metadata::serve(*net, net.meta_port);
    }
}

/// Fires once per incoming connection. Lightyear spawns a child entity carrying `LinkOf` for each
/// client; this is where per-connection components get attached.
fn on_client_connected(trigger: On<Add, LinkOf>, net: Res<NetConfig>, mut commands: Commands) {
    let entity = trigger.entity;
    // ReplicationSender is what lets us replicate local entities to this client.
    let mut connection = commands.entity(entity);
    connection.insert((ReplicationSender, Name::from("Connection"), Authored));
    // Both ends delay only what they receive, so setting the same values on each gives a symmetric
    // link with a round trip of twice the configured latency.
    if let Some(conditioner) = net.conditioner() {
        connection.insert(Link::default().with_conditioner(conditioner));
    }
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

    // Reused on every respawn, so a player always comes back where they started. Server-side
    // only — a client has no use for it.
    let spawn = SpawnIndex(players.iter().count());
    let mut state = PlayerState::default();
    state.position = level::spawn_point(spawn.0);

    commands.spawn((
        Name::from(format!("Player {peer}")),
        Authored,
        Player { peer },
        spawn,
        state,
        Aim::default(),
        Health::default(),
        // Where this client's inputs are written once they arrive.
        ActionState::<PlayerInput>::default(),
        // Replicate is the other half of ReplicationSender: that says the channel may send, this
        // says the entity should be sent.
        Replicate::to_clients(NetworkTarget::All),
        // The two halves of the same decision, and they are complements on purpose. The owner
        // predicts: it simulates this entity locally without waiting for the round trip and rolls
        // back when what arrives disagrees. Everyone else interpolates: they draw it slightly in
        // the past, smoothed between received updates. Nobody gets both, and nobody gets neither.
        PredictionTarget::to_clients(NetworkTarget::Single(remote.0)),
        InterpolationTarget::to_clients(NetworkTarget::AllExceptSingle(remote.0)),
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

