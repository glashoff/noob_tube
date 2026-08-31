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
use avian3d::prelude::Position;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use lightyear::frame_interpolation::prelude::FrameInterpolationSystems;
use lightyear::prelude::{Rollback, RollbackSystems};
use lightyear::prelude::{
    ConfirmedHistory, FrameInterpolate, Interpolated, InterpolationSystems, InterpolationTimeline,
    NetworkTimeline, Predicted, Tick, interpolation_fraction,
};
use noob_tube_shared::player::{PlayerInput, PlayerState, ViewBracket};
use noob_tube_shared::vehicle::Driving;
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
            .register_type::<DrawnView>()
            .init_resource::<DrawnView>()
            .init_resource::<CurrentInput>()
            .init_resource::<ScriptedInput>()
            .init_resource::<MovementTicks>()
            .add_systems(Startup, spawn_player)
            .add_systems(Update, smooth_own_frames)
            .add_systems(
                Update,
                (note_pointer_over_ui, note_drawn_view, grab_cursor, look, sample_input)
                    .chain()
                    // Before interpolation runs again, on purpose. A player reacts to what is on
                    // the screen, and what is on the screen is the blend interpolation produced
                    // *last* frame. Reading it after this frame's update would report a view the
                    // player has not seen yet — half a frame into their own future.
                    .before(InterpolationSystems::Prepare),
            )
            .add_systems(Update, report_drawn_view.after(sample_input))
            // The same step the server runs, over the one entity we predict. Lightyear re-runs
            // this schedule when it rolls back, so this is the replay too.
            .add_systems(
                FixedUpdate,
                (simulation::step_players::<With<Predicted>>, count_ticks),
            )
            // After frame interpolation, not merely before transform propagation. Frame
            // interpolation writes the blended `PlayerState` in PostUpdate, and the camera reads
            // it; the other order would put last frame's fixed value on screen and throw the
            // blend away. Visual correction rides on the same pass, one set later still.
            .init_resource::<ViewError>()
            .add_systems(
                PreUpdate,
                (
                    remember_the_view
                        .after(RollbackSystems::Check)
                        .before(RollbackSystems::Prepare),
                    absorb_the_correction
                        .after(RollbackSystems::Rollback)
                        .before(RollbackSystems::EndRollback),
                )
                    .run_if(resource_exists::<Rollback>),
            )
            // After the frame blend, which writes the simulated pose for this frame, and before the
            // camera reads it.
            .add_systems(
                PostUpdate,
                (crate::vehicle::sit_in_the_seat, smooth_the_view, place_camera)
                    .chain()
                    .after(FrameInterpolationSystems::Interpolate)
                    .before(TransformSystems::Propagate),
            );
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

/// The predicted player, before anyone has asked for it to be drawn between ticks.
type NotYetSmoothed = (With<Predicted>, With<PlayerState>, Without<FrameInterpolate>);

/// Update: asks for the predicted player to be drawn between ticks rather than on them.
///
/// The simulation runs at the tick rate and the screen does not, so without this the eye position
/// only changes on the frames a fixed tick happened to land in — at 64 Hz on a 144 Hz display,
/// roughly every other frame repeats the last one while the view keeps turning smoothly with the
/// mouse. [`FrameInterpolate`] makes lightyear draw the state blended between the last two ticks
/// by the current overstep instead.
///
/// It is also what visual correction is built on: the correction decays the difference between the
/// frame-interpolated pose the last frame drew and the one the replay produced, so without this
/// there is no "what the last frame drew" to compare against.
///
/// Only the predicted entity. Remote players are already smooth for a different reason — they are
/// interpolated between received snapshots — and the confirmed copy is never drawn at all.
fn smooth_own_frames(
    mine: Query<Entity, NotYetSmoothed>,
    mut commands: Commands,
) {
    for entity in mine.iter() {
        commands.entity(entity).insert(FrameInterpolate);
        info!("interpolating our own player between ticks");
    }
}

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
/// [`ScriptedInput`] overrides the keyboard and the mouse when the harness drives the player. The
/// look angles are taken from the player either way, so a script can steer by writing `yaw` while
/// leaving the rest of the input alone.
fn sample_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    // What the screen is showing of everyone else, as of this frame.
    drawn: Res<DrawnView>,
    // Optional so the client still samples input with no window at all — see `windowing` in
    // `main.rs`. A headless client has no cursor to grab, and nothing scripted should wait on one.
    cursor: Option<Single<&CursorOptions, With<PrimaryWindow>>>,
    player: Single<&LocalPlayer>,
    scripted: Res<ScriptedInput>,
    mut input: ResMut<CurrentInput>,
) {
    // The same left click both grabs the cursor and fires, so firing waits until the cursor is
    // already grabbed. Otherwise the click that gives the window focus also empties a round into
    // whatever the camera happened to be pointing at.
    let grabbed = cursor.is_some_and(|cursor| cursor.grab_mode != CursorGrabMode::None);
    let scripted_fire = scripted.0.is_some_and(|scripted| scripted.fire);
    let firing = scripted_fire || (mouse.pressed(MouseButton::Left) && grabbed);
    let bracket = firing.then_some(drawn.0).flatten();

    if let Some(scripted) = scripted.0 {
        input.0 = PlayerInput {
            yaw: player.yaw,
            pitch: player.pitch,
            view: bracket,
            ..scripted
        };
        return;
    }
    input.0 = PlayerInput {
        view: bracket,
        forward: keys.pressed(KeyCode::KeyW),
        backward: keys.pressed(KeyCode::KeyS),
        left: keys.pressed(KeyCode::KeyA),
        right: keys.pressed(KeyCode::KeyD),
        jump: keys.pressed(KeyCode::Space),
        crouch: keys.pressed(KeyCode::ControlLeft),
        fire: firing,
        interact: keys.pressed(KeyCode::KeyE),
        yaw: player.yaw,
        pitch: player.pitch,
    };
}

/// What the screen is currently showing of everyone else, as two received snapshots and a fraction.
///
/// A resource rather than something worked out inside the input system, for two reasons. It is
/// read at a particular moment in the frame — before interpolation runs again — and that is easier
/// to say once, in a system order, than to remember at every use. And it is registered for
/// reflection, so what a client is about to report is readable from outside the process, which is
/// the only way to tell "the bracket is wrong" from "there is no bracket".
///
/// `None` means lightyear has nothing to blend: one snapshot is not a blend. See
/// [`report_drawn_view`] for what that actually indicates.
#[derive(Resource, Default, Reflect, Clone, Copy)]
#[reflect(Resource)]
pub struct DrawnView(pub Option<ViewBracket>);

/// Update: works out the two received snapshots the screen is blending between, and how far along.
///
/// Every interpolated player is drawn from its own [`ConfirmedHistory`], and in principle each has
/// its own bracket. In practice a player who is *moving* produces an update every send interval, so
/// every moving player shares the same one; a player who is not moving produces no updates, and any
/// bracket over a constant position gives the same answer. So one bracket describes the screen. The
/// freshest is the one taken, because a history that has stopped growing belongs to someone who has
/// stopped.
///
/// Ordered before `InterpolationSystems::Prepare`, which is the whole point of reading it here: a
/// player reacts to what is on the screen, and what is on the screen is the blend interpolation
/// produced *last* frame. Reading it after this frame's update would report a view the player has
/// not seen yet.
fn note_drawn_view(
    timeline: Res<InterpolationTimeline>,
    players: Query<&ConfirmedHistory<PlayerState>, With<Interpolated>>,
    // Props are interpolated from their Avian pose, so their histories are separate — and on a
    // server with one player they are the *only* histories there are. Left out, a lone player
    // shooting at a moving crate would report no bracket at all.
    props: Query<&ConfirmedHistory<Position>, With<Interpolated>>,
    mut view: ResMut<DrawnView>,
) {
    let current = timeline.now().tick();
    let overstep = timeline.overstep().to_f32();
    let mut best: Option<ViewBracket> = None;
    let brackets = players
        .iter()
        .filter_map(|history| bracket_ticks(history, current))
        .chain(props.iter().filter_map(|history| bracket_ticks(history, current)));
    for (from, to) in brackets {
        if best.is_some_and(|best| best.to >= to) {
            continue;
        }
        best = Some(ViewBracket {
            from,
            to,
            factor: interpolation_fraction(from, to, current, overstep).clamp(0.0, 1.0),
        });
    }
    view.0 = best;
}

/// The pair of confirmed ticks a history is being blended between at `current`.
///
/// The newest sample at or before now, and the one after it: exactly the pair lightyear picks.
/// Mirroring its choice is the whole point — a different pair would describe a screen nobody saw.
/// `None` when there is no sample after this one, which means the blend has run dry and lightyear
/// is holding the last value: there is a position, but no bracket.
///
/// Generic over the component, so one answer serves a player's state and a prop's pose — two
/// histories with no trait in common but the same shape.
fn bracket_ticks<C: Send + Sync + 'static>(
    history: &ConfirmedHistory<C>,
    current: Tick,
) -> Option<(Tick, Tick)> {
    let previous = (0..history.len())
        .take_while(|index| history.get_nth_tick(*index).is_some_and(|tick| tick <= current))
        .last()?;
    let (from, _) = history.get_nth_state(previous)?;
    let (to, _) = history.get_nth_state(previous + 1)?;
    Some((from, to))
}

/// Update: says once what the first shot actually reported.
///
/// What the shooter reports is the whole of what the server can know about the screen it was aimed
/// at, and a client that reports nothing degrades quietly into a coarser rewind. This is the line
/// that says which happened.
fn report_drawn_view(
    input: Res<CurrentInput>,
    timeline: Res<InterpolationTimeline>,
    players: Query<&ConfirmedHistory<PlayerState>, With<Interpolated>>,
    props: Query<&ConfirmedHistory<Position>, With<Interpolated>>,
    mut reported: Local<bool>,
) {
    if *reported || !input.0.fire {
        return;
    }
    *reported = true;
    match input.0.view {
        Some(view) => info!(
            "first shot reports view ticks {}..{} at {:.2}",
            view.from.0, view.to.0, view.factor,
        ),
        None => {
            // Not a failure of ours: it means lightyear has nothing to blend, because the
            // interpolation timeline has caught up with the newest sample that has arrived. Remote
            // players are being clamped rather than interpolated at that point, which is a problem
            // in its own right and worth saying so plainly.
            let newest = players
                .iter()
                .filter_map(|history| history.get_nth_tick(history.len().checked_sub(1)?))
                .chain(
                    props
                        .iter()
                        .filter_map(|history| history.get_nth_tick(history.len().checked_sub(1)?)),
                )
                .max();
            warn!(
                "first shot reports no view bracket: interpolation is at tick {} but the newest \
                 confirmed sample is {:?} — it is clamping, not blending. The shot falls back to \
                 the coarser rewind.",
                timeline.now().tick().0,
                newest.map(|tick| tick.0),
            );
        }
    }
}

/// FixedUpdate: counts fixed steps, for telling a stalled simulation from a stalled player.
///
/// The two look identical from outside, and one of them cost an afternoon. Note that this now
/// counts replayed ticks as well: a rollback re-runs `FixedMain`, so the number climbing faster
/// than the tick rate is itself the signal that corrections are happening.
fn count_ticks(mut ticks: ResMut<MovementTicks>) {
    ticks.0 += 1;
}

/// Where this client's own player is, and whether they are behind a wheel rather than on foot.
type OwnPose = (&'static PlayerState, Has<Driving>);

/// How far behind the vehicle the camera sits while driving, and how far above it.
///
/// A vehicle is driven from outside it here. There is no cab to sit in — the chassis is one box —
/// and a first-person view from inside a box is a black screen.
const CHASE_DISTANCE: f32 = 7.0;
const CHASE_LIFT: f32 = 1.2;

/// How far the drawn view may fall behind the simulation, in metres.
///
/// This is not a tuning preference, it is the whole trade in one number. Smoothing a correction
/// means drawing the player where they are not, so the size of the error that can be hidden *is*
/// how far behind the view is allowed to get. Anything larger has to show, and should: a view a
/// metre behind puts the crosshair somewhere the player is not, which is worse than the jump it
/// was hiding.
///
/// 25 cm is about 45 ms of running. Measured corrections on a sane link are half a centimetre, so
/// in practice everything is smoothed and nothing is ever clipped by this.
pub const VIEW_LEASH: f32 = 0.25;

/// Fraction of the outstanding view error still left one second later.
///
/// A rate rather than a per-frame factor, so the smoothing does not change with the frame rate.
/// 1e-4 is a time constant of about 110 ms: a correction is most of the way gone in a tenth of a
/// second, which is long enough not to read as a jump and short enough not to be a delay.
const VIEW_DECAY_PER_SECOND: f32 = 1e-4;

/// How far the drawn view currently is from the simulated one, and where it was before a rollback.
///
/// Lightyear can do this itself — `add_linear_correction` — but its `CorrectionPolicy` has private
/// fields and no constructor besides the default, and the decay constant is the entire design.
/// Measured with lightyear's own 200 ms half-life, under a storm of corrections, the view trailed
/// the simulation by 1.9 m: the filter never released, so the camera simply ran a fifth of a second
/// behind. Owning thirty lines is the cheaper way to own that number.
#[derive(Resource, Default)]
pub struct ViewError {
    /// The offset added to the drawn position. Decays towards zero every frame.
    pub offset: Vec3,
    /// Where the view was drawn just before a rollback replaced the state under it.
    drawn: Option<Vec3>,
}

/// PreUpdate, inside the rollback and before it snaps back: where the view was.
///
/// The live `PlayerState` here is last frame's *drawn* value — frame interpolation writes it in
/// `PostUpdate` and only restores the simulated one in `RunFixedMainLoop`, which has not run yet.
/// That is exactly what is wanted: the view has to stay continuous with what was on screen.
fn remember_the_view(mut error: ResMut<ViewError>, drawn: Option<Single<&PlayerState, With<Predicted>>>) {
    error.drawn = drawn.map(|state| state.position);
}

/// PreUpdate, after the replay: takes the difference on to the books.
///
/// The offset is what keeps the drawn position from moving at all this frame; it is then paid off
/// over the following tenth of a second. Clamped, because an error larger than [`VIEW_LEASH`] is
/// one that has to be shown rather than hidden.
fn absorb_the_correction(
    mut error: ResMut<ViewError>,
    simulated: Option<Single<&PlayerState, With<Predicted>>>,
) {
    let (Some(drawn), Some(simulated)) = (error.drawn.take(), simulated) else {
        return;
    };
    error.offset = (drawn - simulated.position).clamp_length_max(VIEW_LEASH);
}

/// PostUpdate: draws the player at the simulated position plus what is still owed.
///
/// Writing to `PlayerState` here changes only what is drawn: `RunFixedMainLoop` restores the
/// simulated value before the next tick, and the shooting code runs in `Update`, after that restore
/// and before this — so a shot still leaves from where the simulation says the player is, however
/// far behind the picture happens to be.
fn smooth_the_view(
    mut error: ResMut<ViewError>,
    time: Res<Time>,
    drawn: Option<Single<&mut PlayerState, With<Predicted>>>,
) {
    error.offset *= VIEW_DECAY_PER_SECOND.powf(time.delta_secs());
    if error.offset.length() < 1e-4 {
        error.offset = Vec3::ZERO;
    }
    if let Some(mut drawn) = drawn {
        drawn.position += error.offset;
    }
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
    predicted: Option<Single<OwnPose, With<Predicted>>>,
    mut camera: Single<(&LocalPlayer, &mut Transform)>,
) {
    let (player, transform) = &mut *camera;
    let look = Quat::from_euler(EulerRot::YXZ, player.yaw, player.pitch, 0.0);
    transform.rotation = look;
    let Some(predicted) = predicted else {
        return;
    };
    let (state, driving) = *predicted;
    transform.translation = if driving {
        // Behind and above, along the direction being looked in rather than the way the vehicle
        // points — so the driver can look around while going straight, which is most of why anyone
        // wants a third-person view in the first place.
        //
        // It does not yet get out of the way of walls: driving backwards into one puts the camera
        // inside it. A chase camera earns its keep by casting a ray and pulling in, and that is
        // its own piece of work.
        state.eye_position() + Vec3::Y * CHASE_LIFT - (look * Vec3::NEG_Z) * CHASE_DISTANCE
    } else {
        state.eye_position()
    };
}
