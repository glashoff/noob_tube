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
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState, ViewBracket};
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::lag_compensation::{PositionHistory, Snapshot};
use noob_tube_shared::shooting::{self, Health, ShotFired};
use noob_tube_shared::protocol::{EffectsChannel, ProtocolPlugin};
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
        // After the step, and in its own schedule so there is no doubt about the order: the
        // history has to hold the position at the *end* of a tick, because that is the one
        // replication sends and therefore the one a client interpolates towards.
        .add_systems(FixedPostUpdate, record_positions)
        // Once per frame, not once per tick: several ticks can resolve between two frames, and
        // there is no reason to touch the network that often for something cosmetic.
        .add_systems(PostUpdate, broadcast_shots)
        .init_resource::<PendingShots>()
        .add_observer(on_client_connected)
        .add_observer(on_peer_connected)
        .add_plugins(remote_inspection())
        .run();
}

/// Which spawn point a player returns to. Server-side, never replicated.
#[derive(Component, Clone, Copy)]
struct SpawnIndex(usize);

/// How far back a shot reaches, and how precisely it can say so.
///
/// The two are not equally good. `Bracket` is what the shooter actually drew: the two confirmed
/// snapshots it was blending and how far between them it was, so the same lerp over the server's
/// own history reproduces that point exactly. `Delay` is the fallback lightyear provides for free —
/// a moment in the past, blended from the two ticks either side of it, which lands somewhere near
/// the drawn point rather than on it.
#[derive(Clone, Copy)]
enum Rewind {
    Bracket(ViewBracket),
    Delay((Tick, f32)),
}

impl Rewind {
    /// The player's position and stance at that moment, out of a history.
    fn apply(&self, history: &PositionHistory) -> Option<Snapshot> {
        match *self {
            Rewind::Bracket(view) => history.sample_bracket(view.from, view.to, view.factor),
            Rewind::Delay((tick, overstep)) => history.sample(tick, overstep),
        }
    }

    /// The earliest tick this reaches back to — how deep the rewind is, for the log.
    fn oldest_tick(&self) -> Tick {
        match *self {
            Rewind::Bracket(view) => view.from,
            Rewind::Delay((tick, _)) => tick,
        }
    }

    /// How it is being asked for, so a log line says which of the two answered.
    fn describe(&self) -> String {
        match *self {
            Rewind::Bracket(view) => {
                format!("ticks {}..{} at {:.2}", view.from.0, view.to.0, view.factor)
            }
            Rewind::Delay((tick, overstep)) => format!("tick {} +{overstep:.2} (from a delay)", tick.0),
        }
    }
}

/// What a shot is fired from: where the shooter is, where they are looking, whether the trigger is
/// down, and which connection to ask how far behind their view was.
type Shooter = (
    Entity,
    &'static PlayerState,
    &'static Aim,
    &'static ActionState<PlayerInput>,
    &'static Player,
    &'static ControlledBy,
);

/// What damage is applied to.
type Wounded = (
    &'static mut Health,
    &'static mut PlayerState,
    &'static Player,
    &'static SpawnIndex,
);

/// FixedUpdate: fires every trigger that is down and ready, and applies the damage.
///
/// Server only. The client predicts its own cooldown, so the weapon answers the trigger without
/// waiting for a round trip, but whether anyone was *hit* is decided here and only here.
///
/// Each shot is tested against the world the shooter was looking at, not the present one — see
/// [`noob_tube_shared::lag_compensation`] for why that is worth the machinery.
///
/// Two passes, because a shooter cannot hold everyone else's health mutably while looking for a
/// target. The first reads; the second writes.
fn resolve_shots(
    world: Res<CollisionWorld>,
    net: Res<NetConfig>,
    timeline: Res<LocalTimeline>,
    histories: Query<&PositionHistory>,
    // How far behind the present each client's view of everyone else is. Lightyear puts this on the
    // connection entity when an input message reports it, which is what `ControlledBy::owner`
    // points at. It is absent until the first message arrives, and absent forever if the client
    // has lag compensation switched off — in both cases the shot falls back to the present.
    delays: Query<&InterpolationDelay>,
    // A ParamSet because the two halves both want `PlayerState` — the first to aim at, the second
    // to respawn. Bevy refuses two queries with overlapping mutable access held at once, and it is
    // right to: the reads below finish before any write starts, but nothing in the signature says
    // so. The set makes that ordering explicit instead of asserted.
    mut players: ParamSet<(Query<Shooter>, Query<Wounded>)>,
    mut pending: ResMut<PendingShots>,
    // Latches the one line below that says whether any of this is actually happening.
    mut reported: Local<bool>,
) {
    let tick = timeline.tick();
    // Everyone who could be hit, gathered once rather than once per shooter. This is the present;
    // each shooter rewinds it to its own moment below.
    let present: Vec<(Entity, Vec3, bool)> = players
        .p0()
        .iter()
        .map(|(entity, state, ..)| (entity, state.position, state.crouching))
        .collect();

    let mut hits: Vec<(Entity, u64)> = Vec::new();
    for (shooter, state, aim, action, player, controlled) in players.p0().iter() {
        if !state.is_firing(&action.0) {
            continue;
        }
        // The moment this shooter's screen was showing when the trigger went down. `tick` is the
        // tick the input was *stamped for*, not the one the packet arrived on, so this is the same
        // answer however late the packet was.
        //
        // Two ways of knowing it, and the first is better wherever it is available. The shooter
        // sends the pair of confirmed ticks it was actually blending between, so the server can
        // rebuild the identical blend; falling back on the interpolation delay means picking a
        // moment and blending the two ticks either side of it, which is not the same point
        // whenever the snapshots the client received were more than one tick apart — that is, at
        // any send rate below the tick rate.
        let rewind = net
            .lag_compensation
            .then(|| match action.0.view {
                Some(view) => Some(Rewind::Bracket(view)),
                None => delays
                    .get(controlled.owner)
                    .ok()
                    .map(|delay| Rewind::Delay(delay.tick_and_overstep(tick))),
            })
            .flatten();

        // Once, on the first shot anyone fires. The server can be configured for lag compensation,
        // keep every position it needs and still rewind nothing, because the other half — the
        // client reporting how far behind its view is — is a separate setting in a separate
        // process. Knowing a setting was read is not knowing it is doing anything, and a first
        // report taken at connect would be measuring clocks that have not settled yet. The first
        // shot is the first moment the number means something.
        if !*reported && net.lag_compensation {
            *reported = true;
            match rewind {
                Some(rewind) => {
                    let back = (tick - rewind.oldest_tick()).max(0) as u32;
                    info!(
                        "lag compensation live: first shot rewound {back} ticks ({:?}), {}",
                        net.tick_duration() * back,
                        rewind.describe(),
                    );
                }
                None => warn!(
                    "lag compensation is on, but peer {} reports no view delay: its shots resolve \
                     against the present. Is lag_compensation off on that client?",
                    player.peer,
                ),
            }
        }

        // Everyone but the shooter. Left in, they would hit themselves at zero distance.
        let targets: Vec<(Entity, Vec3, bool)> = present
            .iter()
            .copied()
            .filter(|(entity, ..)| *entity != shooter)
            .map(|(entity, now, crouching)| {
                let Some(rewind) = rewind else {
                    return (entity, now, crouching);
                };
                let Ok(history) = histories.get(entity) else {
                    return (entity, now, crouching);
                };
                let Some(past) = rewind.apply(history) else {
                    // A player who joined moments ago simply has no such past, which is normal and
                    // passes. A *full* history that still cannot reach back this far is a
                    // configuration that cannot serve this connection, and says so.
                    if history.is_full() {
                        warn!(
                            "lag_comp_history_ticks is too short: peer {} asked to rewind to \
                             {}, oldest kept is {:?}",
                            player.peer,
                            rewind.describe(),
                            history.oldest().map(|tick| tick.0),
                        );
                    }
                    return (entity, now, crouching);
                };
                trace!(
                    "peer {} rewound a target {} ticks to {}: it moved {:.2} m since",
                    player.peer,
                    tick - rewind.oldest_tick(),
                    rewind.describe(),
                    (now - past.position).length(),
                );
                (entity, past.position, past.crouching)
            })
            .collect();

        let (origin, direction) = shooting::aim_ray(state.eye_position(), aim.yaw, aim.pitch);
        let shot = shooting::resolve(&world, origin, direction, targets);
        // Told to everyone, hit or miss: a shot that struck a wall beside you is as much a part of
        // knowing where the fire is coming from as one that struck you.
        pending.0.push(ShotFired {
            shooter: player.peer,
            from: origin,
            to: shot.point(origin, direction),
            hit_player: shot.target.is_some(),
        });
        if let Some(hit) = shot.target {
            debug!("peer {} hit at {:.1} m", player.peer, shot.distance);
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

/// Shots resolved since the last frame, waiting to be told to everyone.
///
/// A queue rather than a message sent from inside `resolve_shots`, because that system runs in the
/// fixed schedule — several times per frame under a fast tick rate, and again for every tick a
/// rollback replays, if the server ever gains one. Sending is a frame-rate concern, not a tick one.
#[derive(Resource, Default)]
struct PendingShots(Vec<ShotFired>);

/// PostUpdate: tells every client what was shot.
///
/// Unreliably, and to everyone including the shooter — see [`ShotFired`]. Failing to send is
/// logged and dropped rather than propagated: a lost tracer must never take down a tick.
fn broadcast_shots(
    mut pending: ResMut<PendingShots>,
    mut sender: ServerMultiMessageSender,
    server: Single<&Server>,
) {
    for shot in pending.0.drain(..) {
        if let Err(error) = sender.send::<_, EffectsChannel>(&shot, *server, &NetworkTarget::All) {
            warn!("could not send a shot effect: {error}");
        }
    }
}

/// FixedPostUpdate: writes this tick's finished position into each player's history.
///
/// The tick recorded is the server's own, which is also the tick a client's inputs are stamped for
/// and the tick replication labels this state with. All three agreeing is what makes a rewind
/// land on the position the shooter actually saw rather than near it.
fn record_positions(
    timeline: Res<LocalTimeline>,
    mut players: Query<(&PlayerState, &mut PositionHistory)>,
) {
    let tick = timeline.tick();
    for (state, mut history) in players.iter_mut() {
        history.record(
            tick,
            Snapshot {
                position: state.position,
                crouching: state.crouching,
            },
        );
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
    net: Res<NetConfig>,
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
        // The past this player can be shot in. Server-side only, like the spawn index.
        PositionHistory::with_capacity(net.lag_comp_history_ticks.into()),
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

