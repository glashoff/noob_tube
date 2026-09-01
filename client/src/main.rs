//! Game client: renders the world, samples input, predicts the local player.

mod bot;
mod character;
mod corrections;
mod crosshair;
mod debug_draw;
mod harness;
mod hud;
mod local_player;
mod props;
mod remote_players;
mod shot_effects;
mod vehicle;
mod world;

use bevy::app::{PluginGroupBuilder, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::ExitCondition;
use bevy::render::settings::{PowerPreference, RenderCreation, WgpuSettings};
use bevy::render::RenderPlugin;
use core::time::Duration;
use lightyear::prelude::*;
use noob_tube_shared::tuning::NetConfig;
use noob_tube_shared::PLACEHOLDER_PRIVATE_KEY;
use std::net::{Ipv4Addr, SocketAddr};

fn main() {
    let net = configure();

    App::new()
        .add_plugins(windowing())
        .insert_resource(Time::<Fixed>::from_hz(net.tick_hz))
        // How far in the past other players are drawn. Inserted before the plugin group, which
        // only fills this in if it is missing.
        .insert_resource(net.interpolation())
        // How far ahead of the present our inputs are stamped, and how far we may predict. The
        // server has no say in this: it acts on whatever tick an input arrives labelled with.
        .insert_resource(net.input_timeline())
        .insert_resource(net)
        .add_plugins((
            noob_tube_shared::physics::PhysicsPlugin,
            // Draws the predicted player between fixed ticks, and carries visual correction.
            lightyear::frame_interpolation::prelude::FrameInterpolationPlugin,
            world::WorldPlugin,
            noob_tube_shared::types::SharedTypesPlugin,
            local_player::LocalPlayerPlugin,
            debug_draw::DebugDrawPlugin,
            crosshair::CrosshairPlugin,
            hud::HudPlugin,
            corrections::CorrectionsPlugin,
            props::PropsPlugin,
            vehicle::VehiclePlugin,
            shot_effects::ShotEffectsPlugin,
            remote_players::RemotePlayersPlugin,
            character::CharacterPlugin,
            harness::HarnessPlugin,
        ))
        // Adds nothing unless `NOOB_TUBE_BOT` is set — the plugin decides that itself, so the
        // condition lives beside the reason for it rather than here.
        .add_plugins(bot::BotPlugin)
        .add_plugins(client::ClientPlugins {
            tick_duration: net.tick_duration(),
        })
        // After the plugin group, before the Client entity is spawned.
        .add_plugins(noob_tube_shared::protocol::ProtocolPlugin { net })
        .add_systems(Startup, connect)
        .add_observer(on_connected)
        .add_plugins(remote_inspection())
        .add_plugins(world_inspector())
        .run();
}

/// The plugin group, with or without a window on someone's desktop.
///
/// `NOOB_TUBE_HEADLESS=1` builds the client with no window at all. It exists because testing needs
/// several clients at once, and every one of them used to pop up over whatever the developer was
/// doing and steal the focus — which on top of being irritating changes the thing under test, since
/// an unfocused window releases the cursor and stops firing.
///
/// Not a hidden window: winit cannot hide one on Wayland. The window is never created, `WinitPlugin`
/// is left out, and a plain loop drives the schedule instead of an event loop. Rendering is still
/// set up, so meshes, materials and the camera all behave — there is simply nowhere for the frames
/// to go. What does *not* work headless is the screenshot harness, which needs a surface.
/// Where the asset server looks, as an absolute path fixed at build time.
///
/// Bevy's default is `assets/` beside the executable, which for a cargo build means
/// `target/debug/assets` — a directory `cargo clean` deletes and nobody would think to put anything
/// in. Anchoring it to the source tree instead puts the assets where they are edited.
///
/// Absolute, and that is the limitation: a binary copied to another machine looks for the path it
/// was built at. Shipping one means making this relative and putting the assets beside it, which is
/// a packaging question and is not one yet.
pub const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../assets");

/// Which graphics adapter to draw with, and why it is not the fast one.
///
/// wgpu asks for `HighPerformance` by default, and on a laptop with two GPUs that means the
/// discrete card. This game does not need it — a few thousand triangles and no post-processing —
/// and on the machine this was written on the discrete card is a GeForce driven by Mesa's NVK,
/// which took the whole machine down twice in an afternoon of running two clients side by side.
/// Nothing here is fast enough to be worth that, so the integrated GPU is the default.
///
/// `WGPU_POWER_PREF=high` overrides it, and is the right thing to reach for on a machine where the
/// discrete driver is solid. Setting the variable at all hands the choice back to wgpu, which is
/// why this only fills in a preference that was not already expressed.
///
/// Two heavier hammers, for when the trouble is the driver being *loaded* rather than used — both
/// are environment, not code, because they are about a machine rather than about this game:
///
/// ```text
/// VK_DRIVER_FILES=/usr/share/vulkan/icd.d/intel_icd.json   # the only Vulkan driver in sight
/// WGPU_BACKEND=gl                                          # skip Vulkan altogether
/// ```
fn draw_with_the_integrated_gpu() -> RenderPlugin {
    let mut wgpu = WgpuSettings::default();
    if PowerPreference::from_env().is_none() {
        wgpu.power_preference = PowerPreference::LowPower;
    }
    RenderPlugin {
        render_creation: RenderCreation::Automatic(Box::new(wgpu)),
        ..default()
    }
}

fn windowing() -> PluginGroupBuilder {
    let plugins = DefaultPlugins
        .set(AssetPlugin {
            file_path: ASSETS.into(),
            ..default()
        })
        .set(draw_with_the_integrated_gpu());

    if std::env::var("NOOB_TUBE_HEADLESS").is_ok_and(|value| value != "0") {
        return plugins
            .set(WindowPlugin {
                primary_window: None,
                // Without this the app exits the moment it notices it has no windows.
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            })
            .disable::<bevy::winit::WinitPlugin>()
            // 240 Hz: fast enough that the fixed timestep never starves, slow enough not to spin a
            // core for nothing.
            .add(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
                1.0 / 240.0,
            )));
    }
    plugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Noob Tube".into(),
            ..default()
        }),
        ..default()
    })
}

/// Reads our own settings, then asks the server for the one it owns.
///
/// The tick rate has to be settled before `App::new`, because it goes into the lightyear plugin
/// group and into `Time<Fixed>`. That is the whole reason the server publishes a metadata endpoint
/// rather than sending it over the game connection: by the time a connection exists, the app is
/// already built around a number.
///
/// A server that does not answer is not an error — plenty will not have the endpoint, and a
/// disagreement still fails safely, as a refused connection rather than a desync. Which of the two
/// happened is worth saying out loud either way.
///
/// `println!` rather than `info!`, and this is the one place it is right: all of this happens
/// before `App::new`, so `LogPlugin` has not installed a tracing subscriber and every `info!` here
/// would go nowhere at all.
fn configure() -> NetConfig {
    let mut net = NetConfig::load();
    if net.meta_port == 0 {
        return net;
    }

    let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), net.meta_port);
    match noob_tube_shared::metadata::fetch(addr) {
        Some(server) => {
            let ours = net.tick_hz;
            net.adopt_from_server(&server);
            if net.tick_hz == ours {
                println!("server at {addr} agrees on {} Hz", net.tick_hz);
            } else {
                println!(
                    "server at {addr} runs {} Hz, adopting it over our {ours}",
                    net.tick_hz
                );
            }
        }
        None => println!(
            "no metadata from {addr}, keeping our {} Hz — a mismatch will refuse to connect",
            net.tick_hz
        ),
    }
    net
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

fn connect(net: Res<NetConfig>, mut commands: Commands) {
    let server_addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), net.port);
    // Port 0 lets the OS pick, so several clients can run on one machine.
    let local_addr = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0);

    let auth = Authentication::Manual {
        server_addr,
        client_id: rand_client_id(),
        private_key: PLACEHOLDER_PRIVATE_KEY,
        protocol_id: net.protocol_id(),
    };

    // Prediction needs its manager resource in place before the client entity exists.
    //
    // The rollback bound is set from the same number that bounds prediction, because there is only
    // one honest answer: a rollback can never need to reach further back than the client is allowed
    // to run ahead. Lightyear keeps the two as separate limits and takes the smaller, and its
    // default of 20 ticks is 312 ms at 64 Hz — so at 300 ms of ping a correction is silently
    // dropped and a predicted body stays wrong forever. That is measured, not theoretical: a
    // predicted crate shoved by a shot froze 10 cm from where the server had it, indefinitely,
    // with no rollback and no warning.
    commands.insert_resource(PredictionManager {
        rollback_policy: RollbackPolicy {
            max_rollback_ticks: net.max_predicted_ticks,
            ..default()
        },
        ..default()
    });

    let client = commands
        .spawn((
            Name::from("Client"),
            noob_tube_shared::types::Authored,
            Client,
            ReplicationReceiver,
            Link::default().with_conditioner(net.conditioner()),
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
    info!("connecting to {server_addr}, {}", net.describe());
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
