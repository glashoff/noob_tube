//! The locally controlled player: mouse look, keyboard input, and the movement step.
//!
//! In M1 this runs entirely locally. M3 replaces the direct application with input sent to the
//! server, and M4 turns it into prediction with reconciliation. The movement itself lives in
//! `shared` and does not change.

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::player::{PlayerInput, PlayerState};

/// Radians of look per pixel of mouse movement.
const MOUSE_SENSITIVITY: f32 = 0.0022;

/// Just short of straight up and down, so the view never flips over.
const PITCH_LIMIT: f32 = core::f32::consts::FRAC_PI_2 - 0.001;

pub struct LocalPlayerPlugin;

impl Plugin for LocalPlayerPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<LocalPlayer>()
            .register_type::<CurrentInput>()
            .register_type::<ScriptedInput>()
            .register_type::<MovementTicks>()
            .init_resource::<CurrentInput>()
            .init_resource::<ScriptedInput>()
            .init_resource::<MovementTicks>()
            .add_systems(Startup, spawn_player)
            .add_systems(Update, (grab_cursor, look, sample_input).chain())
            // Movement runs on the fixed timestep so it ticks at the same rate the server will.
            .add_systems(FixedUpdate, step_movement)
            .add_systems(PostUpdate, place_camera.before(TransformSystems::Propagate));
    }
}

/// The camera entity, which is also the player.
///
/// `Reflect` plus the `#[reflect(Component)]` attribute are what make this readable through the
/// remote inspector; without them the component exists but cannot be named or serialised.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct LocalPlayer {
    pub state: PlayerState,
    pub yaw: f32,
    pub pitch: f32,
}

/// Input gathered this frame, consumed by the fixed-timestep movement step.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
struct CurrentInput(PlayerInput);

/// When set, replaces keyboard input. Used by the harness to drive the player without a human.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct ScriptedInput(pub Option<PlayerInput>);

/// Diagnostics: how many times the fixed movement step has actually run.
///
/// Registered for reflection so it can be read over BRP while the game runs. A stalled simulation
/// and a stalled player look identical from outside; this is what tells them apart.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct MovementTicks(pub u64);

/// Startup: creates the one entity that is both the player and the camera.
///
/// Merging the two is a simplification of M1 that only holds while the local player is the only
/// player and is never seen from outside. M3 splits them, because a replicated player needs a body
/// that other clients can draw and the camera has to be able to detach from it on death.
///
/// The 90 degree field of view is horizontal, matching what the genre has settled on.
fn spawn_player(mut commands: Commands) {
    commands.spawn((
        Name::from("LocalPlayer"),
        LocalPlayer {
            state: PlayerState::default(),
            yaw: 0.0,
            pitch: 0.0,
        },
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
/// In Bevy 0.19 this lives on `CursorOptions`, a component beside `Window`, not a field inside it.
fn grab_cursor(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    cursor: Single<&mut CursorOptions, With<PrimaryWindow>>,
) {
    let mut cursor = cursor.into_inner();
    if mouse.just_pressed(MouseButton::Left) {
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
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
/// Sampling and applying are deliberately separate. This runs per frame, while [`step_movement`]
/// consumes the result on the fixed tick, so a key pressed and released between two ticks can still
/// be seen — and, more importantly, the same `PlayerInput` value is what M3 puts on the wire.
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

/// FixedUpdate: advances the player by exactly one tick.
///
/// This is the only place the player's position changes. It runs on the fixed timestep at
/// `TICK_RATE`, so it may run zero, one or several times in a frame, and `time.delta_secs()` is the
/// constant tick length rather than the frame time. That constancy is the point: the server will
/// step the same function with the same dt, and prediction in M4 replays it — a step that depended
/// on frame rate could not be replayed to the same result.
///
/// [`MovementTicks`] only exists so the harness can tell a stalled simulation from a stalled
/// player. The two look identical from the outside, and one of them cost an afternoon.
fn step_movement(
    input: Res<CurrentInput>,
    world: Option<Res<CollisionWorld>>,
    time: Res<Time<Fixed>>,
    mut ticks: ResMut<MovementTicks>,
    mut player: Single<&mut LocalPlayer>,
) {
    ticks.0 += 1;
    // The collision world appears in Startup, which can land after the first fixed tick.
    let Some(world) = world else { return };
    player
        .state
        .apply_input(&input.0, &world, time.delta_secs());
}

/// PostUpdate: writes the player's eye position and look angles into the camera transform.
///
/// The simulation owns `LocalPlayer`; the transform is a view of it, rewritten from scratch each
/// frame. Nothing reads the transform back, so the two can never disagree.
///
/// Runs in PostUpdate rather than FixedUpdate so the view follows the mouse at the frame rate
/// rather than the tick rate — 64 Hz mouse look feels notably worse than the movement does. It must
/// come before `TransformSystems::Propagate`, or the change lands one frame late in the global
/// transform the renderer reads.
///
/// `EulerRot::YXZ` applies yaw first and pitch second, in the camera's own frame. Any other order
/// makes the horizon tilt as you look up while turning.
fn place_camera(mut player: Single<(&LocalPlayer, &mut Transform)>) {
    let (player, transform) = &mut *player;
    transform.translation = player.state.eye_position();
    transform.rotation =
        Quat::from_euler(EulerRot::YXZ, player.yaw, player.pitch, 0.0);
}
