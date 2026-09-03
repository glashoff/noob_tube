//! Sculpt mode: the brush a player holds, and the strokes it asks for.
//!
//! **F4** turns it on. The brush sits where you are looking, on the ground, and the left button
//! works it. Everything else stays as it is — you walk, you drive, you look around, because the
//! ground you are shaping is ground you have to stand on to judge.
//!
//! Nothing here decides anything about the terrain. It sends a [`Stroke`], which the server checks,
//! charges for and stamps with a tick; the ground moves when that tick comes round, for everybody
//! at once, through [`apply_due_edits`](noob_tube_shared::sculpt::apply_due_edits). So a sculptor
//! sees their own stroke land late — `edit_delay_ticks` late, which at this build's settings is
//! about a sixth of a second. That is deliberate: the commit has to reach everybody before the tick
//! it names. The ring under the brush is what makes the wait legible, since it shows where the
//! stroke is going the moment the button goes down.

use bevy::input::mouse::AccumulatedMouseScroll;
use bevy::prelude::*;
use lightyear::prelude::MessageSender;
use noob_tube_shared::physics::Level;
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::sculpt::{Brush, MAX_LIFT, MAX_RADIUS, Stroke};
use noob_tube_shared::terrain::Ground;

/// How far a sculptor can reach, in metres. Past this the brush has nothing under it.
const REACH: f32 = 200.0;

/// How often a held brush asks for another stroke.
///
/// Not every tick. A held brush at 64 Hz is four times the strokes for no more shaping — the
/// falloff is what makes a stroke soft, not the repetition rate — and every one of them is a
/// message, a broadcast to everyone, and a tile rebuild on every machine.
const STROKES_PER_SECOND: f32 = 20.0;

/// The smallest and largest a brush may be, and what a wheel notch changes it by.
const MIN_RADIUS: f32 = 1.0;
const RADIUS_STEP: f32 = 1.25;
const STRENGTH_STEP: f32 = 1.25;

pub struct SculptingPlugin;

impl Plugin for SculptingPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Chisel>()
            .init_resource::<Chisel>()
            .add_systems(
                Update,
                (turn_it_on, aim_the_brush, work_the_brush, hold_the_trigger)
                    .chain()
                    // After the input has been sampled, because the brush *is* the trigger: it
                    // reads what the player asked for through the same door everything else does,
                    // and `hold_the_trigger` then takes it away again before the world sees it.
                    .after(crate::local_player::sample_input),
            )
            .add_systems(Update, draw_the_brush.after(aim_the_brush))
            .add_systems(Update, name_the_slots.after(turn_it_on));
    }
}

/// How many of the ten hotbar slots the brushes take, from key 1.
///
/// Shared with [`placing`](crate::placing), which fills the rest: the two read different digits and
/// the offset between them has to be one number, not two that agree today.
pub const BRUSH_SLOTS: usize = 4;

/// Which tool, of the four.
///
/// In the order they earn their place rather than the order they are reached for. **Flatten first**:
/// built structures have flat footprints and unsculpted ground does not, so without it every
/// building gets a gap under one corner and buries another.
#[derive(Clone, Copy, PartialEq, Debug, Default, Reflect)]
pub enum Tool {
    #[default]
    Flatten,
    Lift,
    Smooth,
    Ramp,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Flatten => "flatten",
            Tool::Lift => "raise",
            Tool::Smooth => "smooth",
            Tool::Ramp => "ramp",
        }
    }
}

/// What the sculptor is holding.
///
/// Registered for reflection so it can be read over BRP while the game runs — a tool held in the
/// hand has no other way of saying what it is set to, and testing it from outside means being able
/// to ask.
#[derive(Resource, Reflect)]
#[reflect(Resource)]
pub struct Chisel {
    pub on: bool,
    pub tool: Tool,
    pub radius: f32,
    /// Metres a stroke raises, or lowers with the right button held.
    ///
    /// Its own number rather than one strength shared with smoothing, because they are set for
    /// different reasons and in different units — half a metre a stroke is a considered choice
    /// about how fast a hill grows, and it should survive a trip through the smoothing brush.
    pub lift: f32,
    /// How much of the way a smoothing stroke goes, from 0 to 1.
    pub smooth: f32,
    /// The height flatten levels to, picked off the ground with the right button.
    pub height: f32,
    /// Where the brush is, and whether it is on anything at all.
    pub at: Option<Vec3>,
    /// The first end of a ramp, once one has been put down.
    pub anchor: Option<Vec3>,
    /// Seconds left before the held brush asks for another stroke.
    cooldown: f32,
    /// Whether the trigger was down last frame, for the tools that want an edge rather than a hold.
    was_pressed: bool,
}

impl Default for Chisel {
    fn default() -> Self {
        Self {
            on: false,
            tool: Tool::default(),
            radius: 8.0,
            lift: 0.5,
            smooth: 0.5,
            height: 0.0,
            at: None,
            anchor: None,
            cooldown: 0.0,
            was_pressed: false,
        }
    }
}

/// Update: F4 turns sculpting on, and the number keys and the wheel set the brush.
///
/// The wheel rather than a pair of keys, because a brush size is something a hand adjusts
/// continuously while the other hand is busy — and it is multiplicative, so one notch means the
/// same *proportion* at one metre and at fifty.
fn turn_it_on(
    keys: Res<ButtonInput<KeyCode>>,
    wheel: Res<AccumulatedMouseScroll>,
    menu: Res<crate::map_menu::MapMenu>,
    mut chisel: ResMut<Chisel>,
    mut placer: ResMut<crate::placing::Placer>,
) {
    if keys.just_pressed(KeyCode::F4) {
        chisel.on = !chisel.on;
        chisel.anchor = None;
    }
    // The menu owns the keyboard while it is up, and a stray digit typed into a map name must not
    // change the tool behind it.
    if !chisel.on || menu.open {
        return;
    }
    for (key, tool) in [
        (KeyCode::Digit1, Tool::Flatten),
        (KeyCode::Digit2, Tool::Lift),
        (KeyCode::Digit3, Tool::Smooth),
        (KeyCode::Digit4, Tool::Ramp),
    ] {
        if keys.just_pressed(key) {
            chisel.tool = tool;
            chisel.anchor = None;
            // The hand holds one thing: reaching for a brush is putting the placeable down.
            placer.held = None;
        }
    }
    // With a placeable in hand the wheel turns it instead — see `placing::work_the_hand`. The
    // brush's own radius and strength are not what the wheel means then.
    if placer.held.is_some() {
        return;
    }
    let notches = wheel.delta.y;
    if notches != 0.0 {
        let held = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        let factor = if notches > 0.0 { RADIUS_STEP } else { 1.0 / RADIUS_STEP };
        if held {
            // Whichever number the tool in hand actually uses, so shift and the wheel mean "more
            // of this" rather than "more of a number the brush may or may not be reading".
            let factor = if notches > 0.0 { STRENGTH_STEP } else { 1.0 / STRENGTH_STEP };
            match chisel.tool {
                Tool::Lift => chisel.lift = (chisel.lift * factor).clamp(0.01, MAX_LIFT),
                Tool::Smooth => chisel.smooth = (chisel.smooth * factor).clamp(0.02, 1.0),
                // Flatten levels to a height picked off the ground, and a ramp runs between two
                // ends that were clicked. Neither has a strength to turn up.
                Tool::Flatten | Tool::Ramp => {}
            }
        } else {
            chisel.radius = (chisel.radius * factor).clamp(MIN_RADIUS, MAX_RADIUS);
        }
    }
}

/// Update: puts the brush where the sculptor is looking.
///
/// The same ray a shot uses, against the same level geometry, which is what makes the ring land on
/// the ground rather than near it — and it finds the crates and the ramp too, so a brush aimed at
/// one is a brush that has plainly missed the ground.
pub fn aim_the_brush(
    level: Level,
    camera: Option<Single<&GlobalTransform, With<Camera3d>>>,
    mut chisel: ResMut<Chisel>,
) {
    if !chisel.on {
        if chisel.at.is_some() {
            chisel.at = None;
        }
        return;
    }
    let Some(camera) = camera else {
        return;
    };
    let (origin, forward) = (camera.translation(), camera.forward().as_vec3());
    chisel.at = level
        .raycast(origin, forward, REACH)
        .map(|distance| origin + forward * distance);
}

/// Whether the brush is what the hand is holding.
///
/// Two reads that answer one question — the menu owns the keyboard while it is up, and a placeable
/// owns the trigger while it is held — grouped because every system that asks one asks the other.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Hand<'w> {
    menu: Res<'w, crate::map_menu::MapMenu>,
    placer: Res<'w, crate::placing::Placer>,
}

/// Update: works the brush while the button is down.
/// The trigger comes from [`CurrentInput`](crate::local_player::CurrentInput) rather than from the
/// mouse, and that is worth a sentence. It is the same door every other input goes through, so a
/// brush can be driven by the harness and by a bot exactly as a person drives it — and an input no
/// script can reach is an input nobody can test. The right button stays raw, because it is a
/// modifier rather than a trigger and the game has no field for it.
fn work_the_brush(
    input: Res<crate::local_player::CurrentInput>,
    hand: Hand,
    mouse: Res<ButtonInput<MouseButton>>,
    time: Res<Time>,
    ground: Option<Res<Ground>>,
    mut chisel: ResMut<Chisel>,
    sender: Option<Single<&mut MessageSender<Stroke>>>,
) {
    chisel.cooldown = (chisel.cooldown - time.delta_secs()).max(0.0);
    // The trigger belongs to whatever is in the hand, and a placeable is not a brush.
    if !chisel.on || hand.menu.open || ground.is_none() || hand.placer.held.is_some() {
        return;
    }
    let Some(at) = chisel.at else {
        return;
    };

    // The right button is the tool's second half rather than a second tool: for flatten it picks
    // the height to level to, for lift it lowers, and for smooth and ramp it does nothing because
    // there is nothing for it to mean.
    if mouse.just_pressed(MouseButton::Right) {
        match chisel.tool {
            Tool::Flatten => {
                chisel.height = at.y;
                info!("flattening to {:.2} m", at.y);
            }
            Tool::Ramp => chisel.anchor = None,
            _ => {}
        }
    }

    // Ramp is two clicks, not a held button: the first puts an end down, the second lays the run
    // between them. Holding a ramp brush would mean laying the same ramp sixty times a second.
    if chisel.tool == Tool::Ramp {
        // A ramp is two clicks rather than a held button, so it wants the *edge* of the trigger.
        // `fire_cooldown` is a player's business and not a brush's, so the edge is taken here.
        let pressed = input.0.fire;
        let edge = pressed && !chisel.was_pressed;
        chisel.was_pressed = pressed;
        if !edge {
            return;
        }
        let Some(anchor) = chisel.anchor else {
            chisel.anchor = Some(at);
            return;
        };
        chisel.anchor = None;
        let stroke = Stroke {
            at: Vec2::new(anchor.x, anchor.z),
            radius: chisel.radius,
            brush: Brush::Ramp { to: Vec2::new(at.x, at.z), from_y: anchor.y, to_y: at.y },
        };
        if let Some(sender) = sender {
            sender.into_inner().send::<TerrainChannel>(stroke);
        }
        return;
    }

    chisel.was_pressed = input.0.fire;
    let lowering = mouse.pressed(MouseButton::Right) && chisel.tool == Tool::Lift;
    if !input.0.fire && !lowering {
        return;
    }
    if chisel.cooldown > 0.0 {
        return;
    }
    chisel.cooldown = 1.0 / STROKES_PER_SECOND;

    let brush = match chisel.tool {
        Tool::Flatten => Brush::Flatten { height: chisel.height },
        Tool::Lift => {
            let sign = if lowering { -1.0 } else { 1.0 };
            Brush::Lift { metres: (chisel.lift * sign).clamp(-MAX_LIFT, MAX_LIFT) }
        }
        Tool::Smooth => Brush::Smooth { amount: chisel.smooth.clamp(0.0, 1.0) },
        Tool::Ramp => return,
    };
    let stroke = Stroke { at: Vec2::new(at.x, at.z), radius: chisel.radius, brush };
    if let Some(sender) = sender {
        sender.into_inner().send::<TerrainChannel>(stroke);
    }
}

/// Update: the ring under the brush, and the run of a ramp that has one end down.
///
/// Gizmos rather than a mesh, for the reason the debug drawing already uses them: this is a tool
/// overlay and not part of the world, and it changes every frame.
///
/// The ring is drawn on the ground it is over rather than flat at the brush's height, which is what
/// makes it read as a footprint on a hillside instead of a hoop floating through one.
fn draw_the_brush(chisel: Res<Chisel>, ground: Option<Res<Ground>>, mut gizmos: Gizmos) {
    let (Some(at), Some(ground)) = (chisel.at, ground) else {
        return;
    };
    if !chisel.on {
        return;
    }
    let colour = match chisel.tool {
        Tool::Flatten => Color::srgb(0.4, 0.8, 1.0),
        Tool::Lift => Color::srgb(1.0, 0.8, 0.3),
        Tool::Smooth => Color::srgb(0.6, 1.0, 0.6),
        Tool::Ramp => Color::srgb(1.0, 0.5, 0.9),
    };
    let steps = 48;
    let mut previous = None;
    for step in 0..=steps {
        let angle = step as f32 / steps as f32 * std::f32::consts::TAU;
        let (sin, cos) = angle.sin_cos();
        let x = at.x + cos * chisel.radius;
        let z = at.z + sin * chisel.radius;
        let point = Vec3::new(x, ground.0.height_over(x, z) + 0.05, z);
        if let Some(previous) = previous {
            gizmos.line(previous, point, colour);
        }
        previous = Some(point);
    }
    if let Some(anchor) = chisel.anchor {
        gizmos.line(anchor + Vec3::Y * 0.05, at + Vec3::Y * 0.05, colour);
    }
}

/// Update: a sculptor is not shooting.
///
/// The left button is the brush while sculpt mode is on, so the trigger has to be let go of on the
/// way past — otherwise every stroke is also a burst of fire into the hillside being shaped.
/// Everything else about the input is left alone on purpose: walking, driving and looking are how a
/// sculptor judges what they have made.
pub fn hold_the_trigger(
    chisel: Res<Chisel>,
    mut input: ResMut<crate::local_player::CurrentInput>,
) {
    if chisel.on {
        input.0.fire = false;
        input.0.view = None;
    }
}

/// What the sculptor is holding, as a line of text. Used by the HUD.
pub fn readout(chisel: &Chisel) -> String {
    let tool = chisel.tool.name();
    let extra = match chisel.tool {
        Tool::Flatten => format!("to {:.1} m", chisel.height),
        Tool::Lift => format!("{:.2} m a stroke", chisel.lift),
        Tool::Smooth => format!("{:.0}%", chisel.smooth.clamp(0.0, 1.0) * 100.0),
        Tool::Ramp => {
            if chisel.anchor.is_some() { "click the far end".into() } else { "click one end".into() }
        }
    };
    // The two bindings spelled out, because a setting nobody can find is a setting that is not
    // there. The wheel is free of everything else while a brush is in hand, which is what lets one
    // wheel carry both.
    format!(
        "sculpt: {tool}, {extra}   |   wheel: {:.1} m brush   shift+wheel: strength",
        chisel.radius,
    )
}

/// Update: says what the number keys are bound to, for the bar that shows them.
///
/// Here rather than in [`hotbar`](crate::hotbar) because this is the file that *reads* the digits:
/// a label and the key that produces it are one fact, and two files holding it is how a bar comes
/// to promise a key that does nothing.
///
/// Rebuilt every frame rather than edited, so a mode that ends takes its bindings with it.
pub fn name_the_slots(
    chisel: Res<Chisel>,
    placer: Res<crate::placing::Placer>,
    menu: Res<crate::map_menu::MapMenu>,
    mut hotbar: ResMut<crate::hotbar::Hotbar>,
) {
    hotbar.slots.clear();
    // The menu owns the keyboard while it is up, so nothing here is bound; a bar showing tools
    // under keys that are typing a map name would be showing the wrong thing.
    if !chisel.on || menu.open {
        return;
    }
    for tool in [Tool::Flatten, Tool::Lift, Tool::Smooth, Tool::Ramp] {
        let note = match tool {
            Tool::Flatten => format!("to {:.1} m", chisel.height),
            Tool::Lift => format!("{:.2} m", chisel.lift),
            Tool::Smooth => format!("{:.0}%", chisel.smooth.clamp(0.0, 1.0) * 100.0),
            Tool::Ramp => {
                if chisel.anchor.is_some() { "far end".into() } else { "two clicks".into() }
            }
        };
        // Lit only when the brush is what the hand holds: two slots showing as in hand at once
        // would be the bar disagreeing with the game about a thing it exists to report.
        hotbar.add(tool.name(), note, tool == chisel.tool && placer.held.is_none());
    }
}
