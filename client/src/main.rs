//! Game client: renders the world, samples input, predicts the local player.

mod bot;
mod character;
mod corrections;
mod crosshair;
mod debug_draw;
mod grass;
mod ground_material;
mod harness;
mod hotbar;
mod hud;
mod local_player;
mod map_menu;
mod props;
mod recording;
mod remote_players;
mod placing;
mod platform;
mod sculpting;
mod settings;
mod sight;
mod shot_effects;
mod vehicle;
mod water;
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
#[cfg(not(target_family = "wasm"))]
use std::net::ToSocketAddrs;

fn main() {
    // `noob_tube_client server` hosts: this process runs the server as well, on its own thread, and
    // this client connects to it. It exists because a local round otherwise means keeping two
    // builds in step by hand, and the moment shared code changes and only one of them is rebuilt,
    // the two disagree about the protocol. One process is one build, one log, and one window to
    // close when the round is over.
    //
    // Settled first, because everything below is built around what that server says — see
    // [`configure`]. Started last, once the client's `LogPlugin` is in place; see
    // `Prepared::start_beside_the_client`.
    //
    // Not in a browser, where there are no arguments to read and no socket to listen on — and
    // where the server's dependencies do not build at all. See this crate's `Cargo.toml`.
    #[cfg(not(target_family = "wasm"))]
    let hosting = (std::env::args().nth(1).as_deref() == Some("server"))
        .then(noob_tube_server::prepare);
    #[cfg(not(target_family = "wasm"))]
    let hosted = hosting.as_ref().map(noob_tube_server::Prepared::info);
    #[cfg(target_family = "wasm")]
    let hosted = None;

    let (net, dial) = configure(hosted);

    let mut app = App::new();
    app
        .add_plugins(windowing())
        .insert_resource(Time::<Fixed>::from_hz(net.tick_hz))
        // How far in the past other players are drawn. Inserted before the plugin group, which
        // only fills this in if it is missing.
        .insert_resource(net.interpolation())
        // How far ahead of the present our inputs are stamped, and how far we may predict. The
        // server has no say in this: it acts on whatever tick an input arrives labelled with.
        .insert_resource(net.input_timeline())
        .insert_resource(net)
        .insert_resource(dial)
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
        .add_plugins((bot::BotPlugin, map_menu::MapMenuPlugin, placing::PlacingPlugin,
            sculpting::SculptingPlugin, hotbar::HotbarPlugin,
                      ground_material::GroundMaterialPlugin, recording::RecordingPlugin,
                      water::WaterPlugin, grass::GrassPlugin, settings::SettingsPlugin,
                      sight::SightPlugin))
        .add_plugins(client::ClientPlugins {
            tick_duration: net.tick_duration(),
        })
        // After the plugin group, before the Client entity is spawned.
        .add_plugins(noob_tube_shared::protocol::ProtocolPlugin { net })
        .add_systems(Startup, connect)
        .add_observer(on_connected)
        .add_plugins(remote_inspection())
        .add_plugins(world_inspector());

    // The server last, so that the subscriber the plugins above installed is the one it logs
    // through, and so it is listening well before `connect` runs — the window and the renderer
    // still have to come up between here and the first Startup system.
    #[cfg(not(target_family = "wasm"))]
    if let Some(hosting) = hosting {
        hosting.start_beside_the_client();
    }

    app.run();
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
#[cfg(not(target_family = "wasm"))]
pub const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../assets");

/// In a browser it is a URL, relative to the page — the packaging question above, answered by the
/// one platform that forces it. There is no path to be absolute about: every asset is a request to
/// whichever origin served the bundle.
#[cfg(target_family = "wasm")]
pub const ASSETS: &str = "assets";

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

/// How much the client says about itself, and how to ask it for more.
///
/// `NOOB_TUBE_LOG_FILTER` — `?log_filter=` in a browser — is a `tracing` filter, and giving one
/// raises the ceiling to `trace` so that the filter, rather than a level set elsewhere, decides
/// what comes out. It exists because the interesting failures in a dependency are logged at debug:
/// a WebTransport connection that never opens says nothing at all at the default level, on either
/// platform, and there is no way to attach a debugger to a browser tab from a test.
///
/// ```text
/// NOOB_TUBE_LOG_FILTER=info,aeronet=debug,lightyear=debug cargo run -p noob_tube_client
/// http://localhost:8000/?log&log_filter=info,aeronet=debug
/// ```
fn how_loud() -> bevy::log::LogPlugin {
    let Some(filter) = platform::setting("NOOB_TUBE_LOG_FILTER") else {
        return bevy::log::LogPlugin::default();
    };
    bevy::log::LogPlugin {
        filter,
        level: bevy::log::Level::TRACE,
        ..default()
    }
}

fn windowing() -> PluginGroupBuilder {
    let plugins = DefaultPlugins
        .set(AssetPlugin {
            file_path: ASSETS.into(),
            // Bevy looks for a `.meta` file beside every asset it loads. On a disk that is a
            // failed `stat`; over HTTP it is a second request and a 404 for each one, which
            // doubles the traffic of a load and fills the console with failures that are not.
            // Nothing here ships a `.meta` file, so there is nothing to look for.
            #[cfg(target_family = "wasm")]
            meta_check: bevy::asset::AssetMetaCheck::Never,
            ..default()
        })
        .set(draw_with_the_integrated_gpu())
        .set(how_loud());

    if platform::switched_on("NOOB_TUBE_HEADLESS") {
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
            ..where_it_is_drawn()
        }),
        ..default()
    })
}

/// The window fields only a browser has an opinion about.
///
/// It draws on the canvas the page already made, rather than one appended to the body:
/// `index.html` owns the layout, and a canvas that arrives from underneath cannot be styled by the
/// page that is supposed to be sizing it. `fit_canvas_to_parent` is what makes the window follow
/// it. Changing the selector means changing the page too.
#[cfg(target_family = "wasm")]
fn where_it_is_drawn() -> Window {
    Window {
        canvas: Some("#game".into()),
        fit_canvas_to_parent: true,
        ..default()
    }
}

#[cfg(not(target_family = "wasm"))]
fn where_it_is_drawn() -> Window {
    Window::default()
}

/// Reads our own settings, then asks the server for the ones it owns.
///
/// **Everything both ends have to agree on comes from the server**, and this is where it arrives —
/// before `App::new`, because a client is built around these: the tick rate goes into the lightyear
/// plugin group and into `Time<Fixed>`, and the link conditioner goes onto the transport. That is
/// the whole reason the server publishes a metadata endpoint over TCP rather than sending it over
/// the game connection: by the time a connection exists, the app is already built around them. See
/// [`NetConfig::adopt_from_server`] for which settings those are and why the names say so.
///
/// A server that does not answer is not an error — plenty will not have the endpoint — but it is no
/// longer only the tick rate at stake, so the line for it says what is being taken on trust. The
/// tick rate still fails safely, as a refused connection rather than a desync; a link conditioner
/// nobody agreed on fails as a set of numbers that mean nothing, which is quieter and worse.
///
/// A hosted server — `noob_tube_client server` — is passed in rather than asked: it is in this
/// process and has already settled all three answers. Nothing downstream knows the difference.
///
/// `println!` rather than `info!`, and this is the one place it is right: all of this happens
/// before `App::new`, so `LogPlugin` has not installed a tracing subscriber and every `info!` here
/// would go nowhere at all.
fn configure(hosted: Option<noob_tube_shared::metadata::ServerInfo>) -> (NetConfig, Dial) {
    let mut net = NetConfig::load();
    // Our own server is on this machine whatever `NOOB_TUBE_SERVER` says. That variable names the
    // server to *find*, and there is nothing to find when we are the one running it — a client that
    // dialled the address in it would host a round and then join somebody else's.
    let host = if hosted.is_some() { platform::default_host() } else { server_host() };
    let mut dial = Dial {
        // Replaced below by what the server says, when it says anything.
        target: format!("https://{host}:{}", net.port),
        // What to name in the token if the server will not say: the address we resolved for
        // ourselves, which is what this did before the server published one.
        token_addr: fallback_token_addr(net.port),
        cert_digest: String::new(),
    };

    // A hosted server has already answered, in the same process and without a socket. Everything
    // after this point cannot tell the difference, and should not: it is the same three answers.
    let (said, asked) = match hosted {
        Some(info) => (Some(info), "the server in this process".to_string()),
        None => ask_the_server(&net),
    };
    match said {
        Some(server) => {
            dial.token_addr = server.token_addr;
            dial.cert_digest = server.cert_digest;
            // The port comes from the server too, and it has to. `port` is deliberately not one of
            // the settings a client adopts — it is how a client *finds* a server, so taking it from
            // one would be circular. But a browser has no file to read it from and no environment
            // to be told it in: it knows the origin that served the page and nothing else. The one
            // number that cannot be wrong is the port the server is actually listening on, which is
            // the one it just published.
            dial.target = format!("https://{host}:{}", server.token_addr.port());
            match net.adopt_from_server(&server.net) {
                moved if moved.is_empty() => {
                    platform::say(&format!("{asked} agrees with our settings"))
                }
                moved => platform::say(&format!("{asked} says {}", moved.join(", "))),
            }
        }
        None => platform::say(&format!(
            "no metadata from {asked}: our own settings stand, {} Hz and all. A tick rate the \
             server does not share refuses the connection; a simulated link it does not share is \
             not caught by anything. Nor is there a certificate to pin, so a server holding a \
             self-signed one will refuse the connection outright.",
            net.tick_hz,
        )),
    }
    (net, dial)
}

/// Asks the server what it is running, and says where it asked.
///
/// Two platforms, two questions, one answer type. On a desktop this is the socket the server
/// listens on beside the game — see [`noob_tube_shared::metadata`]. In a browser there is no socket
/// to open and nothing may block, so the page has already asked over HTTP and left the answer where
/// [`platform::preloaded_config`] finds it; `meta_port` means nothing there, because the config
/// came from the origin that served the bundle rather than from a port.
///
/// The second half of the pair is only for the log line, and it is a string rather than an address
/// for the same reason: "the page" is where a browser asked.
#[cfg(not(target_family = "wasm"))]
fn ask_the_server(net: &NetConfig) -> (Option<noob_tube_shared::metadata::ServerInfo>, String) {
    if net.meta_port == 0 {
        return (None, "a switched-off metadata endpoint".to_string());
    }
    let addr = server_address(net.meta_port);
    (noob_tube_shared::metadata::fetch(addr), format!("the server at {addr}"))
}

#[cfg(target_family = "wasm")]
fn ask_the_server(_net: &NetConfig) -> (Option<noob_tube_shared::metadata::ServerInfo>, String) {
    (platform::preloaded_config(), "the page".to_string())
}

/// What a connect token names when the server has not said.
///
/// It will not do on the web — a browser has no resolver, and this answers with loopback there —
/// but neither will anything else: without the metadata there is no certificate digest either, and
/// the connection is refused before the token is ever read. It is here so that a desktop client
/// talking to a server with no metadata endpoint behaves as it did before there was one.
#[cfg(not(target_family = "wasm"))]
fn fallback_token_addr(port: u16) -> SocketAddr {
    server_address(port)
}

#[cfg(target_family = "wasm")]
fn fallback_token_addr(port: u16) -> SocketAddr {
    SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
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

/// Where and how to dial the server, settled before the app exists.
///
/// Three separate answers to "which server", and they are separate because a browser cannot derive
/// any of them from the others:
///
/// - [`target`](Self::target) is a URL, and the name in it is what TLS is checked against. A
///   resolved address would not do: a certificate names hosts.
/// - [`token_addr`](Self::token_addr) is an address, because netcode compares it against the one
///   the server bound. The server publishes it, since resolving a name needs a resolver and a
///   browser has none.
/// - [`cert_digest`](Self::cert_digest) is what a self-signed certificate is pinned by, and empty
///   for a publicly trusted one. See the server's `certificate` module.
#[derive(Resource, Clone, Debug)]
struct Dial {
    /// The WebTransport URL, `https://host:port`.
    target: String,
    /// What this client's connect token names.
    token_addr: SocketAddr,
    /// Hex SHA-256 of the certificate to pin, or empty to validate the ordinary way.
    cert_digest: String,
}

/// Which machine the server is on, by name.
///
/// Localhost unless `NOOB_TUBE_SERVER` says otherwise, which is the shape the rest of the settings
/// have: the file is for what you keep, the environment for the one thing you are changing right
/// now. It is not a [`NetConfig`] field because that resource is `Copy` and read by both binaries
/// — a host name is neither a number nor anything the server has an opinion about. `deploy.sh`
/// prints the line to run with it.
///
/// The name, not an address, because this is what goes into the WebTransport URL and a certificate
/// is issued to names.
fn server_host() -> String {
    platform::setting("NOOB_TUBE_SERVER").unwrap_or_else(platform::default_host)
}

/// The same machine, resolved, for a given port.
///
/// Native only: resolving a name needs a resolver, and a browser exposes none. Everything that
/// needed an address there is published by the server instead — see [`Dial`].
///
/// A name is resolved, and only IPv4 answers count: the server binds `0.0.0.0`, so an AAAA record
/// leading the list would produce a connection that times out with nothing to say about why.
#[cfg(not(target_family = "wasm"))]
fn server_address(port: u16) -> SocketAddr {
    let Some(host) = platform::setting("NOOB_TUBE_SERVER") else {
        return SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
    };
    match (host.as_str(), port).to_socket_addrs() {
        Ok(mut found) => match found.find(SocketAddr::is_ipv4) {
            Some(addr) => addr,
            None => {
                println!("{host} has no IPv4 address; falling back to localhost");
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
            }
        },
        Err(error) => {
            println!("cannot resolve {host}: {error}; falling back to localhost");
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)
        }
    }
}

fn connect(net: Res<NetConfig>, dial: Res<Dial>, mut commands: Commands) {
    let auth = Authentication::Manual {
        server_addr: dial.token_addr,
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
            max_rollback_ticks: net.cl_max_predicted_ticks,
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
            client::WebTransportClientIo {
                certificate_digest: dial.cert_digest.clone(),
                target: Some(dial.target.clone()),
            },
            // Not what the URL is built from — `target` above is — but what the rest of the app
            // and an inspector read to say who this link talks to.
            PeerAddr(dial.token_addr),
        ))
        .id();

    commands.trigger(client::Connect { entity: client });
    info!(
        "connecting to {} as {}, {}",
        dial.target,
        dial.token_addr,
        net.describe(noob_tube_shared::tuning::Side::Client),
    );
}

fn on_connected(trigger: On<Add, Connected>) {
    info!("connected: {}", trigger.entity);
}

/// Distinct id per process so two clients on one machine do not collide. See
/// [`platform::unique_id`], which is where the two ways of getting one are written down.
fn rand_client_id() -> u64 {
    platform::unique_id()
}
