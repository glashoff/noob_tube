//! Putting things on the map: what the hand is holding, and the three things it can do with it.
//!
//! **A slot holds a thing, and the button chooses the verb** (terrain.md §7). One slot therefore
//! places, deletes and turns, and the ten keys are not divided by three to make room for the
//! actions. That is the structure worth copying from `webgame`, and it is why there is no "delete
//! crate" tool sitting next to "place crate".
//!
//! The hand holds **one** thing. Keys 1 to 4 are the sculpting brushes and 5 to 0 are placeables,
//! and picking either puts the other down — [`Placer::held`] being `Some` is the whole of that
//! state, which is why there is no third resource saying which mode this is. Sculpting reads it and
//! stands aside; nothing else needs to know.
//!
//! Everything here rides on the sculpting brush's aim. Placement is editing, so it lives behind the
//! same F4, points at the same spot on the ground, and shares the reach — a second ray with its own
//! ideas about where the cursor is would be a second answer to a question with one.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::terrain::{Ground, Marker, MarkerEdit, Palette};

use crate::sculpting::{BRUSH_SLOTS, Chisel};

/// How many slots hold placeables: the ten keys, less the brushes.
pub const PLACE_SLOTS: usize = 10 - BRUSH_SLOTS;

/// How near the aim has to be to a marker before the verbs act on it, in metres.
///
/// Generous, because a marker is a point with nothing to hit: there is no mesh under the crosshair
/// to make the aim feel exact, so the radius is what does. Small enough that two markers a few
/// metres apart are still separable.
const GRAB_RADIUS: f32 = 2.5;

/// Radians one notch of the wheel turns something.
///
/// Fifteen degrees: coarse enough to turn a vehicle round in a few notches, fine enough to line a
/// crate up with a wall without fighting it. A whole number of notches makes a right angle, which
/// is the one alignment anybody actually asks for.
const TURN_STEP: f32 = std::f32::consts::FRAC_PI_2 / 6.0;

pub struct PlacingPlugin;

impl Plugin for PlacingPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Placer>()
            .init_resource::<Placer>()
            .add_systems(
                Update,
                (
                    hear_the_palette,
                    take_the_digits,
                    find_what_is_under_the_aim,
                    work_the_hand,
                )
                    .chain()
                    // The aim is the brush's, so this runs after the ray that sets it — and
                    // before `hold_the_trigger`, which takes the trigger away again so that a
                    // stroke is not also a shot. Placement reads the same trigger, so it has to
                    // read it while it is still there. Without this the placement worked or did
                    // not depending on which order Bevy happened to pick.
                    .after(crate::sculpting::aim_the_brush)
                    .before(crate::sculpting::hold_the_trigger),
            )
            // After sculpting has named its four, because the bar is one row and these are the
            // slots after them.
            .add_systems(Update, name_the_slots.after(crate::sculpting::name_the_slots));
    }
}

/// What the placer is holding, where it is pointing, and which way round the next one goes.
///
/// Registered for reflection so it can be read over BRP while the game runs, for the same reason
/// the brush is: a thing held in the hand has no other way of saying what it is.
#[derive(Resource, Reflect)]
#[reflect(Resource)]
pub struct Placer {
    /// Which placeable each slot holds, from key 5. An empty name is an empty slot.
    pub slots: Vec<String>,
    /// Which slot is in hand, or `None` when the hand holds a sculpting brush instead.
    pub held: Option<usize>,
    /// The rotation the next placement gets. Turned by the wheel, kept between placements so a row
    /// of crates can be laid down all facing the same way.
    pub facing: Quat,
    /// Every kind the server offers, which is what the palette dialog lists.
    pub palette: Vec<String>,
    /// The marker the aim is nearest, when it is near enough to act on.
    pub under: Option<u32>,
    /// Last frame's trigger, so that placing takes the edge rather than the hold.
    was_pressed: bool,
}

impl Default for Placer {
    fn default() -> Self {
        // The three the game has, in the first three slots, so the feature is reachable before
        // anybody has opened the palette. The rest are empty rather than repeats.
        let mut slots = vec![String::new(); PLACE_SLOTS];
        for (slot, kind) in noob_tube_shared::level::PLACEABLES.iter().enumerate() {
            if slot < PLACE_SLOTS {
                slots[slot] = (*kind).to_string();
            }
        }
        Self {
            slots,
            held: None,
            facing: Quat::IDENTITY,
            palette: noob_tube_shared::level::PLACEABLES
                .iter()
                .map(|kind| (*kind).to_string())
                .collect(),
            under: None,
            was_pressed: false,
        }
    }
}

impl Placer {
    /// What is in hand, if anything is.
    pub fn in_hand(&self) -> Option<&str> {
        let slot = self.held?;
        let kind = self.slots.get(slot)?;
        (!kind.is_empty()).then_some(kind.as_str())
    }
}

/// Update: takes the palette the server sent with the map.
///
/// From the resource rather than the message, because the baseline is consumed by `adopt_the_map`
/// before this runs. It is compared rather than assigned every frame so that emptying a slot the
/// server cannot fill happens once, on the change, and not continuously.
fn hear_the_palette(offered: Option<Res<Palette>>, mut placer: ResMut<Placer>) {
    let Some(offered) = offered else {
        return;
    };
    let offered = &offered.0;
    if !offered.is_empty() && placer.palette != *offered {
        placer.palette = offered.clone();
        // A slot holding something this server cannot place would be a slot that refuses every
        // click, with the refusal arriving from the far end of a network. Emptied here instead.
        for slot in placer.slots.iter_mut() {
            if !slot.is_empty() && !offered.contains(slot) {
                slot.clear();
            }
        }
    }
}

/// Update: keys 5 to 0 take a placeable into the hand.
fn take_the_digits(
    keys: Res<ButtonInput<KeyCode>>,
    chisel: Res<Chisel>,
    menu: Res<crate::map_menu::MapMenu>,
    mut placer: ResMut<Placer>,
) {
    // The menu owns the keyboard while it is up: a digit typed into a map name must not reach
    // through it and change what the hand is holding.
    if !chisel.on || menu.open {
        return;
    }
    for (slot, key) in [
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::Digit0,
    ]
    .into_iter()
    .enumerate()
    {
        if keys.just_pressed(key) && slot < placer.slots.len() {
            // An empty slot still takes the hand. Reaching for a slot you have not filled yet and
            // getting the brush back would read as the key not working.
            placer.held = Some(slot);
        }
    }
}

/// Update: which marker the aim is on, if any.
///
/// Nearest within [`GRAB_RADIUS`] of the point the brush's ray landed on, measured on the ground
/// plane. Horizontally rather than in three dimensions, because a marker's height is an offset from
/// ground that may have moved under it, and the aim is a point *on* the ground: comparing the two
/// vertically would lose a crate on a ledge to one buried under it.
fn find_what_is_under_the_aim(
    ground: Option<Res<Ground>>,
    chisel: Res<Chisel>,
    mut placer: ResMut<Placer>,
) {
    let found = match (ground, chisel.at, placer.held.is_some() && chisel.on) {
        (Some(ground), Some(at), true) => ground
            .0
            .markers
            .iter()
            .map(|marker| (marker.id, Vec2::new(marker.x - at.x, marker.z - at.z).length()))
            .filter(|(_, away)| *away <= GRAB_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id),
        _ => None,
    };
    if placer.under != found {
        placer.under = found;
    }
}

/// Whether the hand may act at all, and where it is pointing.
///
/// The three reads that together answer one question — is this client editing, is the keyboard its
/// own, and is there ground under the crosshair — grouped because they are always asked together
/// and never separately.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Aim<'w> {
    ground: Option<Res<'w, Ground>>,
    chisel: Res<'w, Chisel>,
    menu: Res<'w, crate::map_menu::MapMenu>,
}

/// Update: the three verbs.
///
/// Left button places, right button removes what the aim is on, the wheel turns. The wheel turns
/// **what is under the aim** when there is something there and what is in the hand otherwise —
/// which is not a hidden mode but the rule every editor has: the wheel acts on the thing you are
/// pointing at, or on the thing you are holding.
///
/// Nothing here decides whether the edit is allowed. That is the server's, and asking twice would
/// be two rules to keep in step; what comes back is applied by `world::take_placements` whether it
/// was this client that asked or another.
fn work_the_hand(
    input: Res<crate::local_player::CurrentInput>,
    mouse: Res<ButtonInput<MouseButton>>,
    wheel: Res<AccumulatedMouseScroll>,
    keys: Res<ButtonInput<KeyCode>>,
    aim: Aim,
    mut placer: ResMut<Placer>,
    sender: Option<Single<&mut MessageSender<MarkerEdit>>>,
) {
    let pressed = input.0.fire;
    let edge = pressed && !placer.was_pressed;
    placer.was_pressed = pressed;

    if !aim.chisel.on || aim.menu.open || placer.held.is_none() {
        return;
    }
    let (Some(ground), Some(at), Some(sender)) = (aim.ground, aim.chisel.at, sender) else {
        return;
    };
    let mut sender = sender.into_inner();

    // Turning first, because it is the one verb that also applies with nothing in the slot: you can
    // turn a marker that is already down whatever the hand holds.
    let notches = wheel.delta.y;
    let shifted = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
    if notches != 0.0 && shifted {
        let step = Quat::from_rotation_y(TURN_STEP * notches.signum());
        match placer.under.and_then(|id| {
            ground.0.markers.iter().find(|marker| marker.id == id).map(|marker| (id, marker))
        }) {
            // Absolute, not a delta: the current rotation is read here and the *result* is sent, so
            // two authors turning the same vehicle land on one of their two answers rather than on
            // the composition of both — which for rotations is a third answer again.
            Some((id, marker)) => {
                sender.send::<TerrainChannel>(MarkerEdit::Turn {
                    id,
                    rotation: (step * marker.rotation).normalize(),
                });
            }
            None => placer.facing = (step * placer.facing).normalize(),
        }
    }

    if mouse.just_pressed(MouseButton::Right)
        && let Some(id) = placer.under
    {
        sender.send::<TerrainChannel>(MarkerEdit::Remove { id });
        return;
    }

    // An edge rather than the hold: a placement is a decision, and a held button would lay sixty of
    // them a second. The same reasoning the ramp brush already uses for the same button.
    if !edge {
        return;
    }
    let Some(kind) = placer.in_hand() else {
        return;
    };
    sender.send::<TerrainChannel>(MarkerEdit::Place(Marker {
        // The server assigns the handle; this one is a placeholder it overwrites, which is why the
        // question and the answer are different types.
        id: 0,
        kind: kind.to_string(),
        x: at.x,
        z: at.z,
        // On the ground it was aimed at. A height offset is what a marker stores, and zero is the
        // ordinary case — stacking is an author's business and not a click's.
        y: 0.0,
        rotation: placer.facing,
    }));
}

/// What the hand is holding, as a line of text. Used by the HUD.
///
/// Its bindings spelled out for the same reason the brush's are: a verb nobody can find is a verb
/// that is not there, and these three share buttons with things that do something else.
pub fn readout(placer: &Placer) -> Option<String> {
    let slot = placer.held?;
    let kind = placer.slots.get(slot).map(String::as_str).unwrap_or("");
    let facing = placer.facing.to_euler(EulerRot::YXZ).0.to_degrees();
    Some(if kind.is_empty() {
        "place: this slot is empty   |   pick one from the palette".to_string()
    } else {
        format!(
            "place: {kind}, facing {facing:.0}°   |   click to place                right-click removes   shift+wheel turns",
        )
    })
}

/// Update: names the slots after the brushes, for the bar that shows them.
///
/// Here rather than in [`hotbar`](crate::hotbar) for the reason sculpting's half is in sculpting:
/// this is the file that reads these digits, and a label and the key that produces it are one fact.
pub fn name_the_slots(
    placer: Res<Placer>,
    chisel: Res<Chisel>,
    menu: Res<crate::map_menu::MapMenu>,
    mut hotbar: ResMut<crate::hotbar::Hotbar>,
) {
    if !chisel.on || menu.open {
        return;
    }
    for (slot, kind) in placer.slots.iter().enumerate() {
        let held = placer.held == Some(slot);
        let note = if kind.is_empty() {
            String::new()
        } else if held && placer.under.is_some() {
            "on one".into()
        } else {
            format!("{:.0}°", placer.facing.to_euler(EulerRot::YXZ).0.to_degrees())
        };
        let name = if kind.is_empty() { "—" } else { kind.as_str() };
        hotbar.add(name, note, held);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::level::{CRATE, PLACEABLES, PLAYER_SPAWN};

    /// The feature is reachable before anybody has opened the palette.
    ///
    /// An editor whose every slot starts empty is an editor that looks broken: the first thing
    /// somebody does is press a number key, and getting nothing back reads as the key not working
    /// rather than as a slot waiting to be filled.
    #[test]
    fn the_slots_start_holding_what_the_game_has() {
        let placer = Placer::default();
        assert_eq!(placer.slots.len(), PLACE_SLOTS);
        for (slot, kind) in PLACEABLES.iter().enumerate() {
            assert_eq!(placer.slots[slot], *kind);
        }
        assert!(placer.slots[PLACEABLES.len()].is_empty(), "the rest should be empty, not repeats");
        assert_eq!(placer.in_hand(), None, "nothing is held until a key is pressed");
    }

    /// An empty slot takes the hand and holds nothing, which are two different things.
    ///
    /// Reaching for a slot you have not filled has to *feel* like reaching for it — the bar lights
    /// up and the brush goes away — while still placing nothing when the button goes down.
    #[test]
    fn an_empty_slot_can_be_held_and_still_places_nothing() {
        let mut placer = Placer::default();
        let empty = PLACEABLES.len();
        placer.held = Some(empty);
        assert_eq!(placer.in_hand(), None);

        placer.held = Some(0);
        assert_eq!(placer.in_hand(), Some(PLACEABLES[0]));
    }

    /// A slot holding something this server cannot place is emptied, not left to be refused.
    ///
    /// The refusal would otherwise arrive from the far end of a network, once per click, for a
    /// slot that can never work — and the player would have no way to tell that from a bug.
    #[test]
    fn a_slot_the_server_cannot_fill_is_emptied() {
        let mut placer = Placer::default();
        placer.slots[0] = CRATE.into();
        placer.slots[1] = "dragon".into();
        let offered = vec![PLAYER_SPAWN.to_string(), CRATE.to_string()];

        // The body of `hear_the_palette`, which needs no world to be worth checking.
        placer.palette = offered.clone();
        for slot in placer.slots.iter_mut() {
            if !slot.is_empty() && !offered.contains(slot) {
                slot.clear();
            }
        }

        assert_eq!(placer.slots[0], CRATE, "a kind the server has should survive");
        assert!(placer.slots[1].is_empty(), "a kind the server does not have should go");
    }

    /// A turn is a whole number of notches to a right angle.
    ///
    /// The one alignment anybody actually asks for is square to something, so getting there should
    /// not need luck with the wheel.
    #[test]
    fn six_notches_make_a_right_angle() {
        let turned = Quat::from_rotation_y(TURN_STEP * 6.0);
        let square = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        assert!(turned.dot(square).abs() > 1.0 - 1.0e-6, "six notches came to {turned:?}");
    }
}
