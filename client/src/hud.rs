//! What the screen says about itself.
//!
//! One line, bottom left: how far the drawn player has been from the simulated one over the last
//! second, and how much of that the rollback smoothing accounts for. The *player*, not the camera —
//! the camera sits seven metres behind a vehicle while driving, and that is a choice about framing
//! rather than anything being wrong.
//!
//! It is on screen rather than in the log because it is a *feel* question. The measurements in
//! [`corrections`](crate::corrections) say what the numbers are under a scripted walk; this says
//! what they are while somebody is playing, next to the thing they are judging. A number that only
//! exists in a log after the fact cannot be compared with "that felt wrong just then".
//!
//! Two figures, side by side, neither claimed to be part of the other — which was the mistake the
//! first version made. It read "X behind, Y of that smoothing", and the share came out *larger* than
//! the whole: the smoothing offset and the one tick of frame-interpolation delay point in different
//! directions the moment the player turns, and vectors at an angle do not add like numbers.
//!
//! They answer different questions, and only one of them is a fault.
//!
//! **Behind** is the whole distance between the drawn player and the simulated one. On a link with
//! no corrections at all it is not zero, and that surprises people: it is one tick of movement,
//! which is what frame interpolation costs by design. Measured on a perfect link — 5.50 m/s gives
//! 8.5 cm against a tick's 8.59, crouching at 2.60 m/s gives 3.9 against 4.06, and standing still
//! gives 0.00. It scales with speed and vanishes when you stop, because it is a *delay*, not an
//! error.
//!
//! **Correction** is the part left over from a rollback, the one [`VIEW_LEASH`](crate::local_player)
//! governs, and the one that is zero until the client guesses wrong.
//!
//! Top left is the frame rate, which answers a different question again: not whether the drawing
//! agrees with the simulation, but whether there is enough drawing. Both are on screen because a
//! stutter feels the same from the player's chair whichever of them caused it.

use bevy::diagnostic::{DiagnosticsStore, FrameTimeDiagnosticsPlugin};
use bevy::prelude::*;
use noob_tube_shared::tuning::NetConfig;

use crate::corrections::Corrections;
use crate::local_player::VIEW_LEASH;

/// Where the readout sits, in logical pixels from the corner.
const MARGIN: f32 = 12.0;
const FONT_SIZE: f32 = 13.0;
/// Breathing room inside the backing panel.
const PADDING: f32 = 5.0;
/// How often the frame rate is rewritten, in seconds.
const FRAME_RATE_PERIOD: f32 = 1.0;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(FrameTimeDiagnosticsPlugin::default())
            .add_systems(
                Startup,
                (spawn_readout, spawn_frame_rate, spawn_chisel_line, spawn_flight_line),
            )
            .add_systems(
                Update,
                (
                    update_readout,
                    update_frame_rate,
                    toggle_readout,
                    update_chisel_line,
                    update_flight_line,
                ),
            );
    }
}

/// Anything F3 hides. Separate from the two markers below because those have to stay unique —
/// each names exactly one entity, and a `Single` query that matches two matches neither.
#[derive(Component)]
struct Readout;

/// The correction line itself.
#[derive(Component)]
struct ViewReadout;

/// Startup: puts the readout on screen, if there is a screen.
///
/// The window query is what leaves it out of a headless client: no window, no system run, and
/// nothing downstream has to know about the difference.
fn spawn_readout(window: Option<Single<Entity, With<Window>>>, mut commands: Commands) {
    if window.is_none() {
        return;
    }
    commands.spawn((
        Name::from("View readout"),
        Readout,
        ViewReadout,
        Text::default(),
        TextFont {
            font_size: bevy::text::FontSize::Px(FONT_SIZE),
            ..default()
        },
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(MARGIN),
            bottom: Val::Px(MARGIN),
            padding: UiRect::axes(Val::Px(PADDING * 1.4), Val::Px(PADDING)),
            ..default()
        },
        // A panel behind it, because the ground is pale and the sky is dark and a readout has to
        // stay readable over both. Dark and mostly transparent rather than opaque: it is
        // instrumentation, not part of the game, and should not look like a thing in the world.
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
        // Nothing here is interactive, and a node that swallowed clicks would take the click that
        // grabs the cursor with it.
        Pickable::IGNORE,
    ));
}

/// Update: writes this second's figures.
///
/// Coloured by how close the smoothing is to its leash, because that is the number with a
/// threshold: below half of it everything is being hidden, at it the view has stopped hiding and
/// started showing. A figure nobody can read at a glance is a figure nobody reads.
fn update_readout(
    corrections: Res<Corrections>,
    readout: Option<Single<(&mut Text, &mut TextColor), With<ViewReadout>>>,
) {
    let Some(readout) = readout else {
        return;
    };
    let (mut text, mut colour) = readout.into_inner();
    let smoothing = corrections.smoothing_last_second;
    text.0 = format!(
        // ASCII only: the default font has no glyph for a middle dot, and a missing glyph draws
        // as an empty box rather than as nothing.
        "view {:.1} cm behind  |  correction {:.1} cm   (worst frame in the last second)",
        corrections.lag_last_second * 100.0,
        smoothing * 100.0,
    );
    colour.0 = if smoothing >= VIEW_LEASH * 0.99 {
        // On the leash: corrections are arriving faster than they are being paid off, and what is
        // over the leash is being shown as a jump rather than hidden.
        Color::srgb(0.95, 0.35, 0.30)
    } else if smoothing > VIEW_LEASH * 0.5 {
        Color::srgb(0.95, 0.75, 0.30)
    } else {
        Color::srgb(0.75, 0.78, 0.80)
    };
}

/// The frame-rate line.
#[derive(Component)]
struct FrameRate;

/// Startup: puts the frame rate in the top left, if there is a screen.
fn spawn_frame_rate(window: Option<Single<Entity, With<Window>>>, mut commands: Commands) {
    if window.is_none() {
        return;
    }
    commands.spawn((
        Name::from("Frame rate"),
        Readout,
        FrameRate,
        Text::default(),
        TextFont {
            font_size: bevy::text::FontSize::Px(FONT_SIZE),
            ..default()
        },
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(MARGIN),
            top: Val::Px(MARGIN),
            padding: UiRect::axes(Val::Px(PADDING * 1.4), Val::Px(PADDING)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
        Pickable::IGNORE,
    ));
}

/// Update: writes the frame rate, and colours it against the tick rate.
///
/// Against the *tick rate* rather than against 60, because that is the number it means something
/// relative to: the simulation produces a state every tick, and drawing slower than that means
/// states are being computed and never seen. It is also the one threshold in this game that moves
/// — the server owns the tick rate and a client adopts it, so a hard-coded 60 would be right by
/// coincidence.
///
/// The frame time is beside it because that is the figure that says what changed. Frame rate is a
/// reciprocal, so the step from 120 to 60 and the step from 60 to 40 look very different as rates
/// and are the same 8 ms of extra work.
///
/// Rewritten once a second rather than every frame. A number that changes sixty times a second is
/// not a number anybody reads — it is a blur that happens to be near the right value — and the
/// figure behind it is already a rolling average, so a fresh sample every frame says nothing new.
/// It also keeps the readout still enough to be read off a screenshot.
fn update_frame_rate(
    diagnostics: Res<DiagnosticsStore>,
    net: Res<NetConfig>,
    time: Res<Time>,
    mut since: Local<f32>,
    readout: Option<Single<(&mut Text, &mut TextColor), With<FrameRate>>>,
) {
    // Real time, not the fixed timestep: this is about how often a person's eye is asked to take
    // in a new number, which has nothing to do with the simulation's clock.
    *since += time.delta_secs();
    if *since < FRAME_RATE_PERIOD {
        return;
    }
    *since = 0.0;
    let Some(readout) = readout else {
        return;
    };
    let (mut text, mut colour) = readout.into_inner();
    let Some(fps) = diagnostics
        .get(&FrameTimeDiagnosticsPlugin::FPS)
        .and_then(|fps| fps.smoothed())
    else {
        // The first frames, before there is a second measurement to average.
        text.0 = "fps --".into();
        return;
    };
    text.0 = format!("fps {fps:.0}   ({:.1} ms)", 1000.0 / fps.max(1e-3));
    let tick = net.tick_hz;
    colour.0 = if fps < tick * 0.5 {
        Color::srgb(0.95, 0.35, 0.30)
    } else if fps < tick {
        Color::srgb(0.95, 0.75, 0.30)
    } else {
        Color::srgb(0.75, 0.78, 0.80)
    };
}

/// Update: F3 hides it.
///
/// Default on, because it is being watched for; a key to hide it, because it is in the way of a
/// screenshot.
fn toggle_readout(
    keys: Res<ButtonInput<KeyCode>>,
    mut readouts: Query<&mut Visibility, With<Readout>>,
) {
    if !keys.just_pressed(KeyCode::F3) {
        return;
    }
    for mut visibility in readouts.iter_mut() {
        visibility.toggle_visible_hidden();
    }
}


/// The line that says what the brush is set to.
#[derive(Component)]
struct ChiselLine;

/// Startup: puts it above the view readout, if there is a screen.
///
/// Its own line rather than a word appended to the readout below it, because the two answer
/// different questions and only one of them is ever interesting at a time: the readout is
/// instrumentation about the link, and this is the tool in your hand.
fn spawn_chisel_line(window: Option<Single<Entity, With<Window>>>, mut commands: Commands) {
    if window.is_none() {
        return;
    }
    commands.spawn((
        Name::from("Chisel readout"),
        ChiselLine,
        Text::default(),
        TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE), ..default() },
        TextColor(Color::srgb(0.95, 0.85, 0.45)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(MARGIN),
            bottom: Val::Px(MARGIN * 3.4),
            padding: UiRect::axes(Val::Px(PADDING * 1.4), Val::Px(PADDING)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
        Pickable::IGNORE,
        Visibility::Hidden,
    ));
}

/// Update: shows it while there is a brush, and hides it the rest of the time.
fn update_chisel_line(
    chisel: Res<crate::sculpting::Chisel>,
    line: Option<Single<(&mut Text, &mut Visibility), With<ChiselLine>>>,
) {
    let Some(line) = line else {
        return;
    };
    let (mut text, mut visible) = line.into_inner();
    *visible = if chisel.on { Visibility::Visible } else { Visibility::Hidden };
    if chisel.on {
        text.0 = crate::sculpting::readout(&chisel);
    }
}

/// The line that says a player is flying, and how fast.
#[derive(Component)]
struct FlightLine;

/// Startup: puts it above the brush line, if there is a screen.
///
/// A line of its own for the same reason the brush has one: it is a mode the player is in, and
/// the readout below it is instrumentation about the link. It also carries the two keys, because
/// a mode whose controls are only in a commit message is a mode nobody finds.
fn spawn_flight_line(window: Option<Single<Entity, With<Window>>>, mut commands: Commands) {
    if window.is_none() {
        return;
    }
    commands.spawn((
        Name::from("Flight readout"),
        FlightLine,
        Text::default(),
        TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE), ..default() },
        TextColor(Color::srgb(0.55, 0.85, 0.95)),
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(MARGIN),
            bottom: Val::Px(MARGIN * 5.8),
            padding: UiRect::axes(Val::Px(PADDING * 1.4), Val::Px(PADDING)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.45)),
        Pickable::IGNORE,
        Visibility::Hidden,
    ));
}

/// Update: shows it while a player is flying, and hides it the rest of the time.
fn update_flight_line(
    player: Option<Single<&crate::local_player::LocalPlayer>>,
    line: Option<Single<(&mut Text, &mut Visibility), With<FlightLine>>>,
) {
    let (Some(player), Some(line)) = (player, line) else {
        return;
    };
    let (mut text, mut visible) = line.into_inner();
    *visible = if player.flying { Visibility::Visible } else { Visibility::Hidden };
    if player.flying {
        text.0 = format!(
            "flying {:.0} m/s   space up, ctrl down, F to land",
            noob_tube_shared::movement::FLY_SPEED,
        );
    }
}
