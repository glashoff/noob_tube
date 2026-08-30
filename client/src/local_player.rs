//! The locally controlled player: mouse look, keyboard input, and the camera.
//!
//! What this module no longer does is simulate. Since M4 the player *is* the replicated entity the
//! server marked `Predicted`, stepped by the shared
//! [`step_players`](noob_tube_shared::simulation::step_players) and rolled back by lightyear when
//! the server disagrees. There is one simulation now, not two running side by side.
//!
//! What stays here is everything the client owns outright. The look angles are the clearest case:
//! they are input, not state, and a rollback must never touch them — being thrown back a fifth of a
//! second of mouse movement is far worse than the position error it would be fixing.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use lightyear::prelude::Predicted;
use noob_tube_shared::player::{PlayerInput, PlayerState};
use noob_tube_shared::simulation;
use noob_tube_shared::types::Authored;

/// Radians of look per pixel of mouse movement.
const MOUSE_SENSITIVITY: f32 = 0.0022;

/// Just short of straight up and down, so the view never flips over.
const PITCH_LIMIT: f32 = core::f32::consts::FRAC_PI_2 - 0.001;

pub struct LocalPlayerPlugin;

impl Plugin for LocalPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PointerOverUi>()
            // Lightyear counts rollbacks but does not register the type, so nothing outside the
            // process can read it. It is the one number that says whether prediction is agreeing
            // with the server: a steady climb means the client is guessing wrong.
            .register_type::<lightyear::prediction::prelude::PredictionMetrics>()
            .register_type::<LocalPlayer>()
            .register_type::<CurrentInput>()
            .register_type::<ScriptedInput>()
            .register_type::<MovementTicks>()
            .init_resource::<CurrentInput>()
            .init_resource::<ScriptedInput>()
            .init_resource::<MovementTicks>()
            .add_systems(Startup, spawn_player)
            .add_systems(Update, (note_pointer_over_ui, grab_cursor, look, sample_input).chain())
            // The same step the server runs, over the one entity we predict. Lightyear re-runs
            // this schedule when it rolls back, so this is the replay too.
            .add_systems(
                FixedUpdate,
                (simulation::step_players::<With<Predicted>>, count_ticks),
            )
            .add_systems(PostUpdate, place_camera.before(TransformSystems::Propagate));
    }
}

/// The camera, and the look angles that steer it.
///
/// It held the player's `PlayerState` until M4. That state now lives on the predicted entity, which
/// is the server's entity — the camera is a view of it rather than its owner.
///
/// The angles stay here, outside anything replicated, because they are the one part of the player
/// the client is genuinely authoritative over. They travel to the server *as input*; what comes
/// back is a consequence, not a correction.
///
/// `Reflect` plus the `#[reflect(Component)]` attribute are what make this readable through the
/// remote inspector; without them the component exists but cannot be named or serialised.
#[derive(Component, Reflect, Default)]
#[reflect(Component)]
pub struct LocalPlayer {
    pub yaw: f32,
    pub pitch: f32,
}

/// Input gathered this frame, consumed by the fixed-timestep movement step and by the system that
/// hands it to lightyear for sending.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct CurrentInput(pub PlayerInput);

/// When set, replaces keyboard input. Used by the harness to drive the player without a human.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct ScriptedInput(pub Option<PlayerInput>);

/// Set while an inspector panel wants the pointer, so click-to-grab can stand aside.
///
/// Always false without the `inspector` feature — there is no UI to click on.
#[derive(Resource, Default)]
struct PointerOverUi(bool);

/// Update: records whether egui is under the pointer, ahead of [`grab_cursor`].
///
/// A resource rather than querying egui inside `grab_cursor`, so that system needs no `cfg` on its
/// parameters and reads the same either way.
#[cfg(feature = "inspector")]
fn note_pointer_over_ui(
    mut contexts: bevy_inspector_egui::bevy_egui::EguiContexts,
    mut over: ResMut<PointerOverUi>,
) {
    over.0 = contexts
        .ctx_mut()
        .map(|ctx| ctx.egui_wants_pointer_input())
        .unwrap_or(false);
}

#[cfg(not(feature = "inspector"))]
fn note_pointer_over_ui() {}

/// Diagnostics: how many times the fixed movement step has actually run.
///
/// Registered for reflection so it can be read over BRP while the game runs. A stalled simulation
/// and a stalled player look identical from outside; this is what tells them apart.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct MovementTicks(pub u64);

/// Startup: creates the camera.
///
/// It exists from the first frame, before any connection: there is a world to look at long before
/// the server has a player for us, and a camera that appeared on connect would leave the first
/// seconds black.
///
/// The 90 degree field of view is horizontal, matching what the genre has settled on.
fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Name::from("LocalPlayer"),
        Authored,
        LocalPlayer::default(),
        Camera3d::default(),
        Projection::Perspective(PerspectiveProjection {
            fov: 90f32.to_radians(),
            ..default()
        }),
        Transform::default(),
    ));
}

/// Update: locks the cursor on click, releases it on Escape.
///
/// Mouse look reads relative motion, which the OS only keeps delivering once the pointer is locked;
/// unlocked, it stops at the screen edge. Escape has to give it back, or the window cannot be left.
///
/// Three clicks must *not* grab, or the window becomes impossible to work with:
///
/// - one landing outside the client area, which is how a window edge is dragged to resize it;
/// - one on an unfocused window, which is how a window is raised;
/// - one on an inspector panel, which is how its values are edited.
///
/// The first two are what made resizing the window impossible: the grab confined the pointer before
/// it ever reached the edge.
///
/// In Bevy 0.19 this lives on `CursorOptions`, a component beside `Window`, not a field inside it.
fn grab_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    over_ui: Res<PointerOverUi>,
    window: Single<(&Window, &mut CursorOptions), With<PrimaryWindow>>,
) {
    let (window, mut cursor) = window.into_inner();
    let inside = window.cursor_position().is_some();
    if mouse.just_pressed(MouseButton::Left) && inside && window.focused && !over_ui.0 {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
    }
    // Losing focus has to release the pointer too, or alt-tabbing away leaves it captured.
    if !window.focused {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
    if keys.just_pressed(KeyCode::Escape) {
        cursor.grab_mode = CursorGrabMode::None;
        cursor.visible = true;
    }
}

/// Update: turns mouse motion into the player's look angles.
///
/// Runs every frame rather than on the fixed tick, so looking around is as smooth as the display
/// allows. The angles are stored on `LocalPlayer` instead of in the transform because they are also
/// input: [`sample_input`] copies them into the tick's `PlayerInput`, which is what the server will
/// eventually receive.
///
/// `AccumulatedMouseMotion` is the sum of this frame's motion events. Reading the events directly
/// would work too, but it drops motion on frames where several arrive.
///
/// Pitch is clamped just short of straight up and down; at exactly 90 degrees the view flips over.
/// Yaw is left unbounded and simply grows, which `sin_cos` handles for as long as f32 has the
/// precision — several hours of continuous spinning.
fn look(
    motion: Res<AccumulatedMouseMotion>,
    cursor: Single<&CursorOptions, With<PrimaryWindow>>,
    mut player: Single<&mut LocalPlayer>,
) {
    if cursor.grab_mode == CursorGrabMode::None {
        return;
    }
    player.yaw -= motion.delta.x * MOUSE_SENSITIVITY;
    player.pitch = (player.pitch - motion.delta.y * MOUSE_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
}

/// Update: collects this frame's intent into [`CurrentInput`].
///
/// Sampling and using are deliberately separate. This runs per frame, while the fixed tick
/// consumes the result, so a key pressed and released between two ticks can still be seen — and it
/// is the same `PlayerInput` value that goes on the wire.
///
/// `keys.pressed` reports the key being held, not the moment it went down, which is what a movement
/// step wants: holding W has to keep producing forward intent on every tick.
///
/// [`ScriptedInput`] overrides the keyboard when the harness drives the player. The look angles are
/// taken from the player either way, so a script can steer by writing `yaw` while leaving the rest
/// of the input alone.
fn sample_input(
    keys: Res<ButtonInput<KeyCode>>,
    player: Single<&LocalPlayer>,
    scripted: Res<ScriptedInput>,
    mut input: ResMut<CurrentInput>,
) {
    if let Some(scripted) = scripted.0 {
        input.0 = PlayerInput { yaw: player.yaw, pitch: player.pitch, ..scripted };
        return;
    }
    input.0 = PlayerInput {
        forward: keys.pressed(KeyCode::KeyW),
        backward: keys.pressed(KeyCode::KeyS),
        left: keys.pressed(KeyCode::KeyA),
        right: keys.pressed(KeyCode::KeyD),
        jump: keys.pressed(KeyCode::Space),
        crouch: keys.pressed(KeyCode::ControlLeft),
        yaw: player.yaw,
        pitch: player.pitch,
    };
}

/// FixedUpdate: counts fixed steps, for telling a stalled simulation from a stalled player.
///
/// The two look identical from outside, and one of them cost an afternoon. Note that this now
/// counts replayed ticks as well: a rollback re-runs `FixedMain`, so the number climbing faster
/// than the tick rate is itself the signal that corrections are happening.
fn count_ticks(mut ticks: ResMut<MovementTicks>) {
    ticks.0 += 1;
}

/// PostUpdate: writes the predicted eye position and the look angles into the camera transform.
///
/// The two halves come from opposite places, which is the whole shape of prediction in one system.
/// Position comes from the predicted entity, which the server can correct. The angles come from the
/// mouse and are never corrected. Nothing reads the transform back, so it can never disagree with
/// either.
///
/// Until the server has sent us a player there is nothing to stand at, and the camera keeps the
/// position it had — turning on the spot in an empty level for the first fraction of a second.
///
/// Runs in PostUpdate rather than FixedUpdate so the view follows the mouse at the frame rate
/// rather than the tick rate — 64 Hz mouse look feels notably worse than the movement does. It must
/// come before `TransformSystems::Propagate`, or the change lands one frame late in the global
/// transform the renderer reads.
///
/// `EulerRot::YXZ` applies yaw first and pitch second, in the camera's own frame. Any other order
/// makes the horizon tilt as you look up while turning.
fn place_camera(
    predicted: Option<Single<&PlayerState, With<Predicted>>>,
    mut camera: Single<(&LocalPlayer, &mut Transform)>,
) {
    let (player, transform) = &mut *camera;
    if let Some(state) = predicted {
        transform.translation = state.eye_position();
    }
    transform.rotation = Quat::from_euler(EulerRot::YXZ, player.yaw, player.pitch, 0.0);
}
