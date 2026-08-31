//! What the screen says about itself.
//!
//! One line, bottom left: how far the drawn player has been from the simulated one over the last
//! second, and how much of that the rollback smoothing accounts for.
//!
//! It is on screen rather than in the log because it is a *feel* question. The measurements in
//! [`corrections`](crate::corrections) say what the numbers are under a scripted walk; this says
//! what they are while somebody is playing, next to the thing they are judging. A number that only
//! exists in a log after the fact cannot be compared with "that felt wrong just then".
//!
//! One figure, not a breakdown. It was two for a while — the total and "how much of it is
//! smoothing" — and that reads as a decomposition, which it is not: the smoothing offset and the
//! one tick of frame-interpolation delay point in different directions as soon as the player turns,
//! so the share came out *larger* than the whole. Vectors at an angle do not add like numbers. The
//! colour carries the smoothing part instead, where it cannot be read as arithmetic.

use bevy::prelude::*;

use crate::corrections::Corrections;
use crate::local_player::VIEW_LEASH;

/// Where the readout sits, in logical pixels from the corner.
const MARGIN: f32 = 12.0;
const FONT_SIZE: f32 = 13.0;
/// Breathing room inside the backing panel.
const PADDING: f32 = 5.0;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_readout)
            .add_systems(Update, (update_readout, toggle_readout));
    }
}

/// The line itself.
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
        "view off by {:.1} cm  (worst frame in the last second)",
        corrections.lag_last_second * 100.0,
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

/// Update: F3 hides it.
///
/// Default on, because it is being watched for; a key to hide it, because it is in the way of a
/// screenshot.
fn toggle_readout(
    keys: Res<ButtonInput<KeyCode>>,
    readout: Option<Single<&mut Visibility, With<ViewReadout>>>,
) {
    if !keys.just_pressed(KeyCode::F3) {
        return;
    }
    if let Some(mut visibility) = readout {
        visibility.toggle_visible_hidden();
    }
}
