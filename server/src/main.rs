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
use noob_tube_shared::physics::{Layer, Level, PhysicsPlugin};
use avian3d::prelude::{
    Collider, ColliderDensity, CollisionLayers, Forces, LayerMask, LockedAxes, PhysicsSystems,
    Position, RigidBody, Rotation, WriteRigidBodyForces,
};
use noob_tube_shared::hitbox::Hitbox;
use noob_tube_shared::props::{self, Bobbing, Density};
use noob_tube_shared::vehicle::{self, Controls, Driving, VehicleKind};
use noob_tube_shared::lag_compensation::HitboxHistory;
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
        .add_plugins(PhysicsPlugin)
        // The same geometry the client collides against, built from the same numbers. If the two
        // disagreed, every step near the difference would produce a correction the player sees.
        .add_systems(
            Startup,
            (
                start_listening,
                publish_metadata,
                level::spawn_level,
                spawn_props,
                spawn_vehicles,
            ),
        )
        .add_systems(
            FixedUpdate,
            // Before the step, which consumes the trigger by starting the cooldown, and which
            // moves everyone. A shot has to be resolved against the positions its shooter was
            // looking at, not the ones a tick of movement later.
            // The props move with the players, and both after the shots are resolved: a shot is
            // tested against the world as its shooter left it, not one tick of everything later.
            // Suspension alongside the rest: it only applies forces, which the solver in
            // `FixedPostUpdate` then consumes.
            (
                // Getting in and out first: it decides who is walking this tick and who is driving,
                // and both of the steps below depend on that answer.
                (use_vehicles, crates_follow_the_drivers, take_the_wheel).chain(),
                resolve_shots,
                (
                    simulation::step_players::<()>,
                    move_props,
                    vehicle::drive_vehicles::<()>,
                    vehicle::right_flipped_vehicles::<()>,
                ),
            )
                .chain(),
        )
        // After the step, and in its own schedule so there is no doubt about the order: the
        // history has to hold the position at the *end* of a tick, because that is the one
        // replication sends and therefore the one a client interpolates towards.
        // Explicitly after the solver, because Avian runs in this schedule too. Without the
        // ordering the two are ambiguous, and a *dynamic* target — a loose crate, a vehicle —
        // would have its history filled with the pose from before the step on some runs and after
        // it on others. A kinematic crate hid that: nothing moves it except a system of ours.
        .add_systems(
            FixedPostUpdate,
            // A driver is wherever their vehicle is, and the vehicle only reaches this tick's pose
            // in the solver — so this has to come after it, and before the histories are written.
            (carry_drivers, record_positions)
                .chain()
                .after(PhysicsSystems::StepSimulation),
        )
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
    fn apply(&self, history: &HitboxHistory) -> Option<Hitbox> {
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
// Eight parameters, and every one of them is a thing this system genuinely depends on. A Bevy
// system's signature *is* its dependency list, so splitting it to satisfy a count would hide the
// dependencies rather than remove them.
#[allow(clippy::too_many_arguments)]
fn resolve_shots(
    level: Level,
    net: Res<NetConfig>,
    timeline: Res<LocalTimeline>,
    histories: Query<&HitboxHistory>,
    // Props are targets too. A prop offers its own collider and the pose Avian holds, where a
    // player's hitbox is derived from its `PlayerState` — two ways of arriving at the same thing,
    // and the reason a shot does not care which kind of target it met.
    //
    // `CollisionLayers` is read so that the *level* can be left out, and that is not a detail. This
    // query used to take every collider in the world, so the ground and the walls were targets: a
    // shot at a wall came back as a hit, which suppressed its bullet hole and put a hit marker on
    // the shooter's crosshair. The map is already accounted for by `Level::raycast`, which is what
    // stops the bullet; a second reckoning of it can only disagree with the first.
    props: Query<(Entity, &Collider, &Position, &Rotation, &CollisionLayers)>,
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
    // Dynamic props take a shove where they are hit. Reading the pose and writing the velocity,
    // which is why it cannot share a query with anything that also wants them.
    //
    // `RigidBody` comes along because the kind has to be checked, not assumed — see below.
    mut forces: Query<(Forces, &RigidBody)>,
    mut pending: ResMut<PendingShots>,
    // Latches the one line below that says whether any of this is actually happening.
    mut reported: Local<bool>,
) {
    let tick = timeline.tick();
    // Everyone who could be hit, gathered once rather than once per shooter. This is the present;
    // each shooter rewinds it to its own moment below.
    let present: Vec<(Entity, Hitbox)> = players
        .p0()
        .iter()
        .map(|(entity, state, ..)| (entity, Hitbox::of(state)))
        .chain(
            props
                .iter()
                .filter(|(.., layers)| layers.memberships.has_all(Layer::Body))
                .map(|(entity, collider, position, rotation, _)| {
                    (entity, Hitbox::new(collider.clone(), position.0, rotation.0))
                }),
        )
        .collect();

    // Which of those targets are people. A shot into a crate and a shot into a player are the same
    // ray against the same kind of hitbox, and only here does the difference matter: a player takes
    // damage and leaves no decal, a crate takes a shove and does.
    let people: bevy::platform::collections::HashSet<Entity> =
        players.p0().iter().map(|(entity, ..)| entity).collect();

    let mut hits: Vec<(Entity, u64)> = Vec::new();
    for (shooter, state, action, player, controlled) in players.p0().iter() {
        // The same predicate `shooting::fire` applies below, asked early so that a player who is
        // not shooting does not pay for a rewind of everyone else.
        if !state.is_firing(&action.0) {
            continue;
        }
        let input = action.0;
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
        let targets: Vec<(Entity, Hitbox)> = present
            .iter()
            .filter(|(entity, _)| *entity != shooter)
            .map(|(entity, now)| {
                let (entity, now) = (*entity, now.clone());
                let Some(rewind) = rewind else {
                    return (entity, now);
                };
                let Ok(history) = histories.get(entity) else {
                    return (entity, now);
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
                    return (entity, now);
                };
                trace!(
                    "peer {} rewound a target {} ticks to {}: it moved {:.2} m since",
                    player.peer,
                    tick - rewind.oldest_tick(),
                    rewind.describe(),
                    (now.centre() - past.centre()).length(),
                );
                (entity, past)
            })
            .collect();

        // The same call the shooter's own client makes to draw its tracer, over the same input.
        let Some(fired) = shooting::fire(&level, state, &input, targets) else {
            continue;
        };
        // Told to everyone, hit or miss: a shot that struck a wall beside you is as much a part of
        // knowing where the fire is coming from as one that struck you.
        pending.0.push(ShotFired {
            shooter: player.peer,
            from: fired.origin,
            to: fired.point(),
            hit_player: fired.shot.target.is_some_and(|target| people.contains(&target)),
        });
        if let Some(hit) = fired.shot.target {
            debug!("peer {} hit at {:.1} m", player.peer, fired.shot.distance);
            hits.push((hit, player.peer));
            // A shove where it landed, not at the centre, so a corner hit spins the crate.
            //
            // Only a *dynamic* body, and the check is not a formality. A kinematic body matches
            // `Forces` perfectly well — it has every component in that query — and Avian integrates
            // its velocities like any other, so an impulse at a corner set the bobbing crates
            // turning, which a crate on rails must never do. A player is genuinely not matched:
            // players are not rigid bodies at all.
            if let Ok((mut body, kind)) = forces.get_mut(hit)
                && kind.is_dynamic()
            {
                body.apply_linear_impulse_at_point(
                    fired.direction * shooting::WEAPON_IMPULSE,
                    fired.point(),
                );
            }
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

/// Startup: puts the moving crates in the world.
///
/// A kinematic Avian body: it moves where the server puts it and nothing pushes back, which is what
/// an animation is. It carries the rule it moves by (server-side), the shape it presents (sent once)
/// and a history to be rewound out of — the same three things a player has.
///
/// [`Layer::Body`] rather than `Layer::Level` is what keeps it from becoming terrain. The level
/// queries a player's feet make only see `Layer::Level`, so a crate stops bullets without stopping
/// anyone walking under it.
fn spawn_props(net: Res<NetConfig>, mut commands: Commands) {
    // Loose crates: dynamic, so they fall, stack and take a shove. The first thing here the
    // solver actually works for, and the first thing a client simulates that is not its own player.
    let loose = props::loose_prop();
    for (index, at) in props::LOOSE_CRATES.into_iter().enumerate() {
        commands.spawn((
            Name::from(format!("Loose crate {index}")),
            Authored,
            Loose,
            loose,
            RigidBody::Dynamic,
            loose.collider(),
            ColliderDensity(props::LOOSE_DENSITY),
            // The same number, sent once, so a client that has to predict this crate weighs it the
            // same way rather than assuming there is only one kind.
            Density(props::LOOSE_DENSITY),
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
            Position(at),
            HitboxHistory::with_capacity(net.lag_comp_history_ticks.into()),
            Replicate::to_clients(NetworkTarget::All),
            // Interpolated, not predicted, and that is the rule rather than a shortcut. A client
            // can only predict what it has the information to compute, and what moves a crate is
            // *somebody else's* shot — which it learns about no sooner than the server tells it.
            // Predicting it therefore means guessing, being wrong, and snapping: measured at a
            // median of 3.7 cm and up to 79 cm per correction. Interpolation is never wrong about
            // what it draws; it merely draws it late, and lag compensation is exactly the machinery
            // that undoes "late". See the README, "Things that move and are not players".
            InterpolationTarget::to_clients(NetworkTarget::All),
        ));
    }
    info!("{} loose crates", props::LOOSE_CRATES.len());

    for (index, crate_) in props::MOVING_CRATES.into_iter().enumerate() {
        let prop = crate_.prop();
        commands.spawn((
            Name::from(format!("Moving crate {index}")),
            Authored,
            crate_,
            prop,
            RigidBody::Kinematic,
            // A crate on rails does not turn. Belt and braces beside the check in `resolve_shots`:
            // that one stops the impulse that was turning them, this one states the intent on the
            // entity, so the next thing that reaches for a kinematic body's velocity cannot spin it
            // either.
            LockedAxes::ROTATION_LOCKED,
            prop.collider(),
            CollisionLayers::new(Layer::Body, LayerMask::ALL),
            Position(crate_.position_at(0.0)),
            HitboxHistory::with_capacity(net.lag_comp_history_ticks.into()),
            Replicate::to_clients(NetworkTarget::All),
            // Interpolated by everyone and predicted by nobody. There is no input behind a prop to
            // predict from, and no client simulates one.
            InterpolationTarget::to_clients(NetworkTarget::All),
        ));
    }
    info!("{} moving crates", props::MOVING_CRATES.len());
}

/// How close you have to be to get into a vehicle, in metres.
///
/// Generous. Reaching for a door handle is not the interesting part of this, and a radius that has
/// to be hunted for turns a one-key action into a game of its own.
const REACH: f32 = 4.0;

/// On a crate that the solver moves, as opposed to one on rails.
///
/// Server-side. A client tells the two apart by what it has been asked to do with them, which is
/// the only difference that matters to it.
#[derive(Component, Clone, Copy)]
struct Loose;

/// On a vehicle: who is driving it. Server-side; a client works it out from what it predicts.
#[derive(Component, Clone, Copy)]
struct Driver(Entity);

/// On a player: the connection they came in on.
///
/// [`Player::peer`] is the netcode id and reads the same, but replication is addressed by
/// [`PeerId`], and rebuilding one from the number would be a guess about which variant it came out
/// of. This is the one the server was handed.
#[derive(Component, Clone, Copy)]
struct Owner(PeerId);

/// Everything getting in or out of a vehicle needs to know about a player.
type Reaching = (
    Entity,
    &'static ActionState<PlayerInput>,
    &'static mut PlayerState,
    &'static Owner,
    Has<Driving>,
    &'static mut Interacted,
);

/// On a player: whether the interact key was down last tick.
///
/// Getting in is an *edge*, not a state, and an input carries only the state. Holding the key would
/// otherwise climb in and out again sixty-four times a second.
#[derive(Component, Default)]
struct Interacted(bool);

/// A vehicle, from outside it: where it is, which way round, and whether the seat is taken.
type Parked = (
    Entity,
    &'static Position,
    &'static Rotation,
    Option<&'static Driver>,
);

/// FixedUpdate: gets people in and out of vehicles.
///
/// Not predicted, and that is deliberate rather than lazy. Whether a seat is free is the server's
/// to decide — two players reaching for the same door on the same tick have to be resolved
/// somewhere, and a client that guessed would have to be taken back out again. The cost is that the
/// camera changes half a round trip after the key, which for something that happens once a minute
/// is a fair price for never having to un-seat anyone.
fn use_vehicles(
    mut players: Query<Reaching>,
    mut vehicles: Query<Parked, With<VehicleKind>>,
    mut commands: Commands,
) {
    for (player, action, mut state, owner, driving, mut last) in players.iter_mut() {
        let pressed = action.0.interact;
        let edge = pressed && !last.0;
        last.0 = pressed;
        if !edge {
            continue;
        }

        if driving {
            let Some((vehicle, position, rotation, _)) = vehicles
                .iter()
                .find(|(.., driver)| driver.is_some_and(|driver| driver.0 == player))
            else {
                continue;
            };
            // Put down beside the driver's door, clear of the bodywork so the first tick on foot is
            // not spent being pushed out of a wall.
            state.position = position.0 + rotation.0 * Vec3::new(-2.0, -0.6, 0.0);
            state.velocity = Vec3::ZERO;
            commands.entity(player).remove::<Driving>();
            commands.entity(vehicle).remove::<Driver>();
            // Nobody predicts it again: with no input behind it there is nothing to predict from.
            commands.entity(vehicle).insert((
                PredictionTarget::to_clients(NetworkTarget::None),
                InterpolationTarget::to_clients(NetworkTarget::All),
            ));
            info!("{:?} got out", owner.0);
            continue;
        }

        let nearest = vehicles
            .iter_mut()
            .filter(|(.., driver)| driver.is_none())
            .map(|(entity, position, ..)| (entity, position.0.distance(state.position)))
            .filter(|(_, distance)| *distance <= REACH)
            .min_by(|(_, a), (_, b)| a.total_cmp(b));
        let Some((vehicle, _)) = nearest else {
            continue;
        };
        commands.entity(player).insert(Driving);
        commands.entity(vehicle).insert((
            Driver(player),
            Controls::default(),
            // The driver predicts it and everyone else interpolates it — the same split a player
            // gets, for the same reason: the input that moves it is theirs, so they are the one peer
            // that can compute where it will be without being told.
            PredictionTarget::to_clients(NetworkTarget::Single(owner.0)),
            InterpolationTarget::to_clients(NetworkTarget::AllExceptSingle(owner.0)),
        ));
        info!("{:?} got in", owner.0);
    }
}

/// FixedUpdate: hands the loose crates to whoever is driving, and takes them back afterwards.
///
/// A driver has to predict the crates, and the reason is a measurement rather than a preference.
/// An interpolated crate is `RigidBody::Static` on a client and stands at the pose it had a
/// round trip ago, so a predicted vehicle drives into an immovable wall in the past while the
/// server pushes straight through it: four seconds of that produced **245 rollbacks — one per
/// server update — moving the vehicle by up to 7.4 m**. Predicted, both sides shove the same crate
/// on the same tick and there is nothing to correct.
///
/// It is not a retraction of the rule that a crate is interpolated. The rule is that a peer
/// predicts what it has the information to compute, and behind the wheel that is exactly what the
/// driver has: the crate is moved by *their* bumper. The information they lack is somebody else's
/// shot, which arrives as a correction — measured earlier at a median of 3.7 cm — and which the
/// driver is in the worst position to care about, since nobody shoots from the driver's seat.
///
/// Everyone who is *not* driving keeps the interpolated crate, and with it a rewound hitbox that is
/// exactly where the server says it was. That is the half of the trade worth protecting.
///
/// Written only when the set of drivers changes. Replication components are not free to churn, and
/// this would otherwise rewrite four crates sixty-four times a second to say the same thing.
fn crates_follow_the_drivers(
    drivers: Query<&Owner, With<Driving>>,
    crates: Query<Entity, With<Loose>>,
    mut last: Local<Vec<PeerId>>,
    mut commands: Commands,
) {
    let now: Vec<PeerId> = drivers.iter().map(|owner| owner.0).collect();
    // Compared as a set rather than a list, because a query's order is not a promise and a
    // reordering is not a change. `PeerId` is not `Ord`, and for the handful of drivers a server
    // has, a scan beats reaching for a hash set.
    if now.len() == last.len() && now.iter().all(|peer| last.contains(peer)) {
        return;
    }
    *last = now.clone();

    // `Only`/`AllExcept` rather than the single-peer pair the vehicle uses: there can be as many
    // drivers as there are vehicles, and every one of them needs the same crates.
    let (predicted, interpolated) = if now.is_empty() {
        (NetworkTarget::None, NetworkTarget::All)
    } else {
        (
            NetworkTarget::Only(now.iter().copied().collect()),
            NetworkTarget::AllExcept(now.iter().copied().collect()),
        )
    };
    for entity in crates.iter() {
        commands.entity(entity).insert((
            PredictionTarget::to_clients(predicted.clone()),
            InterpolationTarget::to_clients(interpolated.clone()),
        ));
    }
    info!("{} driver(s) now predict the loose crates", now.len());
}

/// FixedUpdate: hands each vehicle the input of whoever is sitting in it.
///
/// The one place the two sides differ, and only in how they find the driver: the server looks up
/// the seat, a client uses its own input because the only vehicle it predicts is the one it is
/// driving. Everything after this reads [`Controls`] and cannot tell the difference.
fn take_the_wheel(
    time: Res<Time<Fixed>>,
    drivers: Query<&ActionState<PlayerInput>>,
    mut vehicles: Query<(&VehicleKind, &Driver, &mut Controls)>,
) {
    let dt = time.delta_secs();
    for (kind, driver, mut controls) in vehicles.iter_mut() {
        let Ok(action) = drivers.get(driver.0) else {
            continue;
        };
        controls.apply_input(kind.spec(), &action.0, dt);
    }
}

/// FixedPostUpdate: a driver is wherever their vehicle ended up.
///
/// Their `PlayerState` is still the thing everything else reads — where a shot comes from, what a
/// hitbox is built from, where they respawn — so it has to keep meaning something while they are
/// not walking. It means "in that seat".
fn carry_drivers(
    vehicles: Query<(&Position, &Rotation, &Driver)>,
    mut players: Query<&mut PlayerState, With<Driving>>,
) {
    for (position, rotation, driver) in vehicles.iter() {
        let Ok(mut state) = players.get_mut(driver.0) else {
            continue;
        };
        // Feet on the floor of the cab rather than at the centre of the body, so the eye ends up
        // roughly where a head would be.
        state.position = position.0 + rotation.0 * Vec3::new(0.0, -0.4, 0.0);
        state.velocity = Vec3::ZERO;
    }
}

/// Startup: puts one vehicle in the world.
///
/// Replicated and interpolated by everyone, predicted by nobody — for now. There is no driver, so
/// there is no input to predict from, and a body nobody steers is exactly the case where the server
/// deciding alone is both cheaper and right. The moment someone is behind the wheel that changes,
/// and it changes for that one client only.
///
/// It gets a [`HitboxHistory`] for the same reason the moving crate does: everyone else sees it in
/// the past, so a shot at it has to be tested against the past. The hitbox itself needs no new code
/// — a vehicle offers its collider and its pose, and `resolve_shots` never asks what kind of thing
/// it hit.
fn spawn_vehicles(net: Res<NetConfig>, mut commands: Commands) {
    let kind = VehicleKind::Buggy;
    for (index, (ground, yaw)) in level::VEHICLE_STARTS.into_iter().enumerate() {
        // Standing at its own ride height, so it starts resting on its springs rather than dropping
        // on to them in front of everyone at the start of the round.
        let at = ground.extend(kind.spec().ride_height()).xzy();
        commands.spawn((
            Name::from(format!("Buggy {index}")),
            Authored,
            vehicle::vehicle_body(kind, at, Quat::from_rotation_y(yaw)),
            HitboxHistory::with_capacity(net.lag_comp_history_ticks.into()),
            Replicate::to_clients(NetworkTarget::All),
            // Nobody predicts an empty vehicle: with no input behind it there is nothing to predict
            // from. `use_vehicles` moves this the moment somebody climbs in, per vehicle, so a
            // second one changes nothing here.
            InterpolationTarget::to_clients(NetworkTarget::All),
        ));
    }
    info!("{} vehicles", level::VEHICLE_STARTS.len());
}

/// FixedUpdate: advances every prop to where this tick says it should be.
///
/// From the tick, not by accumulating: a server that stalled for a moment comes back where the
/// crate belongs rather than that much behind, and the history then agrees with the rule that made
/// it.
fn move_props(
    timeline: Res<LocalTimeline>,
    net: Res<NetConfig>,
    mut props: Query<(&Bobbing, &mut Position)>,
) {
    let seconds = timeline.tick().0 as f32 * net.tick_duration().as_secs_f32();
    for (prop, mut position) in props.iter_mut() {
        position.0 = prop.position_at(seconds);
    }
}

/// FixedPostUpdate: writes this tick's finished position into each player's history.
///
/// The tick recorded is the server's own, which is also the tick a client's inputs are stamped for
/// and the tick replication labels this state with. All three agreeing is what makes a rewind
/// land on the position the shooter actually saw rather than near it.
fn record_positions(
    timeline: Res<LocalTimeline>,
    mut players: Query<(&PlayerState, &mut HitboxHistory)>,
    mut props: Query<(&Collider, &Position, &Rotation, &mut HitboxHistory), Without<PlayerState>>,
) {
    let tick = timeline.tick();
    for (state, mut history) in players.iter_mut() {
        history.record(tick, Hitbox::of(state));
    }
    // A prop's hitbox is its own collider at the pose Avian holds, where a player's comes out of a
    // `PlayerState`. Both are read at the same moment in the tick, which is what makes the two
    // histories comparable. Recording a collider costs nothing to speak of: the shape is shared.
    for (collider, position, rotation, mut history) in props.iter_mut() {
        history.record(tick, Hitbox::new(collider.clone(), position.0, rotation.0));
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
        Owner(remote.0),
        Interacted::default(),
        spawn,
        state,
        Aim::default(),
        Health::default(),
        // The past this player can be shot in. Server-side only, like the spawn index.
        HitboxHistory::with_capacity(net.lag_comp_history_ticks.into()),
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

