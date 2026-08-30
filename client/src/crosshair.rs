//! The crosshair.
//!
//! A shooter without one is unplayable: the weapon fires down the centre of the screen and nothing
//! on the screen says where the centre is. This is UI rather than a world-space object, because it
//! is not in the world — it does not move, occlude, or take part in anything.
//!
//! Four arms and a gap rather than a full cross. The gap is the point: a solid cross covers the one
//! pixel you are trying to look at, which at range is the whole target.
//!
//! Every arm is drawn twice, a black shape a pixel larger behind a white one, because a plain white
//! crosshair disappears against a bright wall and a plain black one against a shadow. Later children
//! draw over earlier ones, so spawn order is the stacking order — every outline goes down first.

use bevy::prelude::*;

/// Half the empty space at the centre, in logical pixels.
const GAP: f32 = 4.0;
/// How long each arm is.
const LENGTH: f32 = 7.0;
/// How thick each arm is.
const THICKNESS: f32 = 2.0;
/// How far the outline extends past its arm on every side.
const OUTLINE: f32 = 1.0;

/// How long the hit marker stays up. Long enough to register, short enough that two hits in quick
/// succession read as two rather than as one long one.
const HIT_MARKER_SECONDS: f32 = 0.14;

pub struct CrosshairPlugin;

impl Plugin for CrosshairPlugin {
    fn build(&self, app: &mut App) {
        app.add_observer(on_hit_landed)
            .add_systems(Startup, spawn_crosshair)
            .add_systems(Update, fade_hit_marker);
    }
}

/// The zero-sized node at the centre of the screen that every arm hangs off.
///
/// Anything else that wants to draw at the exact centre — a hit marker, a hint — is a child of
/// this, and gets the centring for free.
#[derive(Component)]
pub struct Crosshair;

/// Where each arm sits relative to the centre, and which way round it is.
const ARMS: [(f32, f32, bool); 4] = [
    (-(GAP + LENGTH), -THICKNESS / 2.0, true),
    (GAP, -THICKNESS / 2.0, true),
    (-THICKNESS / 2.0, -(GAP + LENGTH), false),
    (-THICKNESS / 2.0, GAP, false),
];

/// Startup: builds the crosshair, if there is a screen to put it on.
///
/// The window query is what leaves it out of a headless client: no window, no system run, and
/// nothing downstream has to know about the difference.
fn spawn_crosshair(window: Option<Single<Entity, With<Window>>>, mut commands: Commands) {
    if window.is_none() {
        return;
    }
    commands
        .spawn((
            Name::from("HUD"),
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            // Nothing here is interactive, and a full-screen node that swallowed clicks would take
            // the click that grabs the cursor with it.
            Pickable::IGNORE,
        ))
        .with_children(|hud| {
            hud.spawn((Name::from("Crosshair"), Crosshair, Node::default(), Pickable::IGNORE))
                .with_children(|centre| {
                    for outline in [true, false] {
                        for (dx, dy, horizontal) in ARMS {
                            let (width, height) = if horizontal {
                                (LENGTH, THICKNESS)
                            } else {
                                (THICKNESS, LENGTH)
                            };
                            centre.spawn(arm(dx, dy, width, height, outline));
                        }
                    }
                });
        });
}

/// The four diagonal ticks that say one of our shots connected.
///
/// The one piece of feedback a shooter genuinely needs. Everything else about a shot — the tracer,
/// the hole in the wall — is for everyone; this is only for the person who pulled the trigger, and
/// it answers the question they are actually asking, which is whether that did anything.
#[derive(Component)]
struct HitMarker {
    /// Seconds left before it goes away.
    remaining: f32,
}

/// Where the four hit-marker ticks sit, in the diagonal gaps the crosshair arms leave free.
const TICKS: [(f32, f32); 4] = [(-9.0, -9.0), (4.0, -9.0), (-9.0, 4.0), (4.0, 4.0)];
/// How big each one is.
const TICK_SIZE: f32 = 5.0;

/// One of our shots landed on someone.
///
/// An event rather than a call, so that whatever learns a shot landed does not also have to know
/// how the crosshair is built — and so that anything else wanting to react to a hit, a sound most
/// obviously, can do so without going through here.
#[derive(Event)]
pub struct HitLanded;

/// Puts the hit marker up, or refreshes the one already there.
///
/// Refreshing rather than spawning a second, so a burst that lands three times shows one marker
/// being renewed instead of three drawn on top of each other.
fn on_hit_landed(
    _: On<HitLanded>,
    crosshair: Option<Single<Entity, With<Crosshair>>>,
    mut existing: Query<&mut HitMarker>,
    mut commands: Commands,
) {
    if let Some(mut marker) = existing.iter_mut().next() {
        marker.remaining = HIT_MARKER_SECONDS;
        return;
    }
    let Some(crosshair) = crosshair else { return };
    commands.entity(*crosshair).with_children(|centre| {
        centre
            .spawn((
                Name::from("Hit marker"),
                HitMarker { remaining: HIT_MARKER_SECONDS },
                Node::default(),
                Pickable::IGNORE,
            ))
            .with_children(|marker| {
                // White over black, like the arms, and for the same reason: the players are red, so
                // a red marker on the body it just hit is the one place it would be invisible.
                for outline in [true, false] {
                    for (dx, dy) in TICKS {
                        marker.spawn(arm(dx, dy, TICK_SIZE, TICK_SIZE, outline));
                    }
                }
            });
    });
}

/// Update: counts each hit marker down and takes it away.
fn fade_hit_marker(
    time: Res<Time>,
    mut markers: Query<(Entity, &mut HitMarker)>,
    mut commands: Commands,
) {
    for (entity, mut marker) in markers.iter_mut() {
        marker.remaining -= time.delta_secs();
        if marker.remaining <= 0.0 {
            commands.entity(entity).despawn();
        }
    }
}

/// One arm of the crosshair, or the black shape sitting behind it.
///
/// `dx`/`dy` are offsets from the exact centre of the screen. The outline grows by [`OUTLINE`] on
/// every side, so it has to start that much further out to stay concentric.
fn arm(dx: f32, dy: f32, width: f32, height: f32, outline: bool) -> impl Bundle {
    let grow = if outline { OUTLINE } else { 0.0 };
    let colour = if outline {
        Color::srgba(0.0, 0.0, 0.0, 0.85)
    } else {
        Color::srgba(1.0, 1.0, 1.0, 0.9)
    };
    (
        Node {
            position_type: PositionType::Absolute,
            left: Val::Px(dx - grow),
            top: Val::Px(dy - grow),
            width: Val::Px(width + grow * 2.0),
            height: Val::Px(height + grow * 2.0),
            ..default()
        },
        BackgroundColor(colour),
        Pickable::IGNORE,
    )
}
