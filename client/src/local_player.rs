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
        app.init_resource::<CurrentInput>()
            .init_resource::<ScriptedInput>()
            .add_systems(Startup, spawn_player)
            .add_systems(Update, (grab_cursor, look, sample_input).chain())
            // Movement runs on the fixed timestep so it ticks at the same rate the server will.
            .add_systems(FixedUpdate, step_movement)
            .add_systems(PostUpdate, place_camera.before(TransformSystems::Propagate));
    }
}

/// The camera entity, which is also the player.
#[derive(Component)]
pub struct LocalPlayer {
    pub state: PlayerState,
    pub yaw: f32,
    pub pitch: f32,
}

/// Input gathered this frame, consumed by the fixed-timestep movement step.
#[derive(Resource, Default)]
struct CurrentInput(PlayerInput);

/// When set, replaces keyboard input. Used by the harness to drive the player without a human.
#[derive(Resource, Default)]
pub struct ScriptedInput(pub Option<PlayerInput>);

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

/// Locks the cursor on click, releases it on Escape.
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

fn step_movement(
    input: Res<CurrentInput>,
    world: Option<Res<CollisionWorld>>,
    time: Res<Time<Fixed>>,
    mut player: Single<&mut LocalPlayer>,
) {
    // The collision world appears in Startup, which can land after the first fixed tick.
    let Some(world) = world else { return };
    player
        .state
        .apply_input(&input.0, &world, time.delta_secs());
}

/// Writes the player's eye position and look angles into the camera transform.
///
/// Runs in PostUpdate rather than FixedUpdate so the view follows the mouse at the frame rate
/// rather than the tick rate — 64 Hz mouse look feels notably worse than the movement does.
fn place_camera(mut player: Single<(&LocalPlayer, &mut Transform)>) {
    let (player, transform) = &mut *player;
    transform.translation = player.state.eye_position();
    transform.rotation =
        Quat::from_euler(EulerRot::YXZ, player.yaw, player.pitch, 0.0);
}
