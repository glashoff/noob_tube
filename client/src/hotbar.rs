//! The number keys, and what they are bound to right now.
//!
//! A row of slots along the bottom of the screen, one per number key, the one in hand lit. Taken
//! from `webgame`, which puts its inventory there for the reason every game with a hotbar does: a
//! key with a number on it is only usable if you can see what the number means without stopping to
//! remember.
//!
//! **It shows bindings rather than owning them.** [`Hotbar`] is filled each frame by whoever the
//! digits currently belong to — [`sculpting`](crate::sculpting) for the brushes on 1 to 4 and
//! [`placing`](crate::placing) for the placeables on 5 to 0, each of them the code that also
//! *reads* those digits, so a label and the key that produces it are written in one place and
//! cannot drift apart. Nothing bound, nothing shown: an empty bar is hidden rather than drawn
//! as ten empty boxes promising keys that do nothing.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

/// How a slot is drawn.
const SLOT_WIDTH: f32 = 92.0;
const SLOT_HEIGHT: f32 = 52.0;
const SLOT_GAP: f32 = 6.0;
const BOTTOM: f32 = 16.0;

/// How much room the bar takes along the bottom of the screen, in logical pixels.
///
/// Read by the [`hud`](crate::hud), whose readouts stand in the same corner. The row is ten slots
/// wide and centred, so in a small window it reaches the left edge — and a readout at the margin is
/// then printed straight through the tools, which is exactly what a hotbar is for looking at.
pub const CLEARANCE: f32 = BOTTOM + SLOT_HEIGHT;
const NAME_SIZE: f32 = 14.0;
const NOTE_SIZE: f32 = 11.0;
const KEY_SIZE: f32 = 10.0;

pub struct HotbarPlugin;

impl Plugin for HotbarPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Hotbar>()
            .init_resource::<Hotbar>()
            .add_systems(Startup, spawn_bar)
            // After everything that might fill it, so what is drawn is this frame's bindings and
            // not last frame's. `Update` ordering by set would be tidier the day a second thing
            // owns the digits; with one filler, being late is enough.
            // After *everything* that fills it. Two things own the digits now — the brushes and
            // the placeables — and painting after only the first drew a four-slot bar for a game
            // that had ten.
            .add_systems(
                Update,
                (rebuild, paint)
                    .chain()
                    .after(crate::sculpting::name_the_slots)
                    .after(crate::placing::name_the_slots),
            );
    }
}

/// One number key's worth of binding.
#[derive(Clone, Debug, Default, Reflect)]
pub struct Slot {
    /// What the key selects.
    pub name: String,
    /// What it is set to, in as few characters as will say it. Empty for a slot with no setting.
    pub note: String,
    /// Whether this is the one in hand.
    pub active: bool,
}

/// What the number keys do, in order from 1.
///
/// Cleared and refilled every frame by whoever owns the digits, rather than edited: a bar that is
/// rebuilt from the truth cannot hold a binding that has been taken away. Registered for
/// reflection so what the keys are bound to can be read over BRP — the same reason the menu and
/// the brush are.
#[derive(Resource, Default, Reflect)]
#[reflect(Resource)]
pub struct Hotbar {
    pub slots: Vec<Slot>,
}

impl Hotbar {
    /// Puts one slot on the bar. Called in key order.
    pub fn add(&mut self, name: impl Into<String>, note: impl Into<String>, active: bool) {
        self.slots.push(Slot { name: name.into(), note: note.into(), active });
    }
}

/// The row itself.
#[derive(Component)]
struct Bar;

/// One box in it, by its place from the left — which is also its key, less one.
#[derive(Component)]
struct BarSlot(usize);

/// Startup: builds the empty row, if there is a screen to put it on.
///
/// The window query is what leaves it out of a headless client, the same guard the HUD and the
/// menu use.
fn spawn_bar(windows: Query<(), With<PrimaryWindow>>, mut commands: Commands) {
    if windows.is_empty() {
        return;
    }
    commands.spawn((
        Name::from("Hotbar"),
        Bar,
        Node {
            position_type: PositionType::Absolute,
            bottom: Val::Px(BOTTOM),
            left: Val::Px(0.0),
            right: Val::Px(0.0),
            justify_content: JustifyContent::Center,
            column_gap: Val::Px(SLOT_GAP),
            ..default()
        },
        // Nothing here is interactive, and a node that swallowed clicks would take the click that
        // works the brush with it.
        Pickable::IGNORE,
        Visibility::Hidden,
    ));
}

/// Update: makes the boxes match the number of bindings.
///
/// Only when the count changes. Which tool is in hand and what it is set to change constantly and
/// are painted onto the boxes; how many there are changes when a mode does, which is rare.
fn rebuild(
    hotbar: Res<Hotbar>,
    bar: Option<Single<Entity, With<Bar>>>,
    mut built: Local<usize>,
    mut commands: Commands,
) {
    let Some(bar) = bar else { return };
    if *built == hotbar.slots.len() {
        return;
    }
    *built = hotbar.slots.len();

    let bar = bar.into_inner();
    commands.entity(bar).despawn_related::<Children>();
    for index in 0..hotbar.slots.len() {
        commands.entity(bar).with_children(|bar| {
            bar.spawn((
                BarSlot(index),
                Node {
                    width: Val::Px(SLOT_WIDTH),
                    height: Val::Px(SLOT_HEIGHT),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.5)),
                BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.18)),
                Pickable::IGNORE,
            ))
            .with_children(|slot| {
                // The key first and small, in the corner it would be printed in on a keyboard.
                slot.spawn((
                    Text::new(format!("{}", key_of(index))),
                    TextFont { font_size: bevy::text::FontSize::Px(KEY_SIZE), ..default() },
                    TextColor(Color::srgb(0.55, 0.58, 0.62)),
                    Node {
                        position_type: PositionType::Absolute,
                        top: Val::Px(3.0),
                        left: Val::Px(5.0),
                        ..default()
                    },
                    Pickable::IGNORE,
                ));
                slot.spawn((
                    Text::default(),
                    TextFont { font_size: bevy::text::FontSize::Px(NAME_SIZE), ..default() },
                    TextColor(Color::WHITE),
                    Pickable::IGNORE,
                ));
                slot.spawn((
                    Text::default(),
                    TextFont { font_size: bevy::text::FontSize::Px(NOTE_SIZE), ..default() },
                    TextColor(Color::srgb(0.6, 0.68, 0.78)),
                    Pickable::IGNORE,
                ));
            });
        });
    }
}

/// Which key a place on the bar belongs to.
///
/// Ten slots, and the tenth is 0 — where the row of digits actually ends. Past that there is no
/// key, and a slot nobody can reach is not drawn.
fn key_of(index: usize) -> u8 {
    if index == 9 { 0 } else { index as u8 + 1 }
}

/// Update: writes the bindings onto the boxes.
fn paint(
    hotbar: Res<Hotbar>,
    bar: Option<Single<&mut Visibility, With<Bar>>>,
    mut slots: Query<(&BarSlot, &Children, &mut BackgroundColor, &mut BorderColor)>,
    mut texts: Query<(&mut Text, &mut TextColor)>,
) {
    if let Some(bar) = bar {
        *bar.into_inner() =
            if hotbar.slots.is_empty() { Visibility::Hidden } else { Visibility::Visible };
    }
    for (slot, children, mut background, mut border) in slots.iter_mut() {
        let Some(binding) = hotbar.slots.get(slot.0) else { continue };
        background.0 = if binding.active {
            Color::srgba(0.35, 0.45, 0.6, 0.7)
        } else {
            Color::srgba(0.0, 0.0, 0.0, 0.5)
        };
        *border = BorderColor::all(if binding.active {
            Color::srgba(0.95, 0.85, 0.45, 0.9)
        } else {
            Color::srgba(1.0, 1.0, 1.0, 0.18)
        });
        // Spawn order: the key, then the name, then the note. The key never changes.
        for (place, child) in children.iter().enumerate() {
            let Ok((mut text, mut colour)) = texts.get_mut(child) else { continue };
            match place {
                1 => {
                    text.0 = binding.name.clone();
                    colour.0 = if binding.active {
                        Color::WHITE
                    } else {
                        Color::srgb(0.78, 0.8, 0.83)
                    };
                }
                2 => text.0 = binding.note.clone(),
                _ => {}
            }
        }
    }
}
