//! The map menu: what maps there are, which one this is, and how to make another.
//!
//! **A menu, not a HUD.** It is modal and opened deliberately, because every action in it is
//! disruptive to everybody — a load moves every player on the server onto different ground. So it
//! takes the screen, takes the keyboard, and gives the cursor back; nothing in it annotates the
//! game while the game is being played.
//!
//! It decides nothing. Every rule about what a map may be lives on the server and in `shared`, and
//! this asks — which is why the form validates against
//! [`Grid::new`](noob_tube_shared::terrain::Grid::new) as it is typed rather than against a second
//! copy of the caps. The numbers under the form are the ones the caps are enforced against, and an
//! author should watch them move.
//!
//! Function keys and modifiers throughout, and that is not decoration: the fields take typing, so
//! any plain letter used as a command would be a letter that cannot be typed into a map name.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use lightyear::prelude::{MessageReceiver, MessageSender, MessageSystems};
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::terrain::{
    Grid, MapList, MapRequest, MAX_BASELINE_BYTES, sanitise_name,
};

/// Where the panel sits and how big the type is.
const MARGIN: f32 = 40.0;
const FONT_SIZE: f32 = 15.0;
const PADDING: f32 = 18.0;

pub struct MapMenuPlugin;

impl Plugin for MapMenuPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<MapMenu>()
            .init_resource::<MapMenu>()
            .add_systems(Startup, spawn_panel)
            // After lightyear has filled the receivers, for the same reason the baseline's reader
            // is: an inbox read before that is always empty.
            .add_systems(PreUpdate, hear_the_server.after(MessageSystems::Receive))
            .add_systems(Update, (open_and_close, edit).chain())
            .add_systems(Update, draw.after(edit))
            // Immediately after the keyboard is read into the game's input, and never before: the
            // point is to overwrite what was sampled, not to race it.
            .add_systems(
                Update,
                take_the_keyboard.after(crate::local_player::sample_input),
            );
    }
}

/// One thing that can be typed into.
///
/// Extent and spacing rather than sample counts, because those are what somebody making a map
/// actually thinks in — and the counts are derived and shown back, since they are what the caps
/// are enforced against.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Name,
    ExtentX,
    ExtentZ,
    Spacing,
    Low,
    High,
}

const FIELDS: [(Field, &str); 6] = [
    (Field::Name, "Name"),
    (Field::ExtentX, "Extent X"),
    (Field::ExtentZ, "Extent Z"),
    (Field::Spacing, "Spacing"),
    (Field::Low, "Lowest"),
    (Field::High, "Highest"),
];

/// The menu, and everything it is showing.
///
/// Registered for reflection, so what it is showing can be read over BRP while the game runs. That
/// is not decoration: a modal driven entirely by the keyboard has no other way of saying which
/// field the typing is going into, and testing it from outside means being able to ask.
#[derive(Resource, Reflect)]
#[reflect(Resource)]
pub struct MapMenu {
    pub open: bool,
    /// The last thing the server said. Never edited here — this is a copy of the server's answer,
    /// and the server answers every request, so it is never stale for longer than a round trip.
    known: MapList,
    /// Which map in the list the arrows are on.
    selected: usize,
    /// Which field the typing goes into.
    focus: usize,
    values: Vec<String>,
}

impl Default for MapMenu {
    fn default() -> Self {
        Self {
            open: false,
            known: MapList::default(),
            selected: 0,
            focus: 0,
            // The built-in map's own numbers, so the first map somebody makes is a sensible one
            // and the form starts in a state that passes its own caps.
            values: ["", "512", "512", "1", "-64", "64"].map(String::from).to_vec(),
        }
    }
}

impl MapMenu {
    fn value(&self, field: Field) -> &str {
        FIELDS
            .iter()
            .position(|(which, _)| *which == field)
            .map(|index| self.values[index].as_str())
            .unwrap_or_default()
    }

    fn number(&self, field: Field) -> f32 {
        self.value(field).trim().parse().unwrap_or(f32::NAN)
    }

    /// The grid the form describes, or why it is not one.
    ///
    /// Through `Grid::new`, which is the server's own gate. A menu that greyed out over-cap inputs
    /// against its own idea of the caps would be a second copy to drift from the first, and the
    /// drift would show up as a request that looks fine here and is refused there.
    fn described(&self) -> Result<Grid, String> {
        Grid::new(
            self.number(Field::ExtentX),
            self.number(Field::ExtentZ),
            self.number(Field::Spacing),
            self.number(Field::Low),
            self.number(Field::High),
        )
        .map_err(|fault| fault.to_string())
    }

    fn request_to_create(&self) -> MapRequest {
        MapRequest::Create {
            name: self.value(Field::Name).to_string(),
            extent_x: self.number(Field::ExtentX),
            extent_z: self.number(Field::ExtentZ),
            spacing: self.number(Field::Spacing),
            min_y: self.number(Field::Low),
            max_y: self.number(Field::High),
        }
    }
}

/// The panel itself.
#[derive(Component)]
struct MenuPanel;

/// The one text node inside it.
#[derive(Component)]
struct MenuText;

/// Startup: builds the panel, if there is a screen to put it on.
///
/// The window query is what leaves it out of a headless client — no window, no system run, and no
/// menu nobody can see. The same guard the HUD uses.
fn spawn_panel(windows: Query<(), With<PrimaryWindow>>, mut commands: Commands) {
    if windows.is_empty() {
        return;
    }
    commands
        .spawn((
            Name::from("Map menu"),
            MenuPanel,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Px(MARGIN),
                top: Val::Px(MARGIN),
                right: Val::Px(MARGIN),
                bottom: Val::Px(MARGIN),
                padding: UiRect::all(Val::Px(PADDING)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.05, 0.05, 0.07, 0.94)),
            Visibility::Hidden,
        ))
        .with_children(|panel| {
            panel.spawn((
                MenuText,
                Text::new(String::new()),
                TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE), ..default() },
                TextColor(Color::srgb(0.86, 0.88, 0.9)),
            ));
        });
}

/// PreUpdate: takes the server's answer.
///
/// Every request is answered, including the ones that fail, so a menu that asked for something and
/// heard nothing back would be looking at a lost packet — which this channel does not have.
fn hear_the_server(mut inbox: Query<&mut MessageReceiver<MapList>>, mut menu: ResMut<MapMenu>) {
    for mut receiver in inbox.iter_mut() {
        for list in receiver.receive() {
            if let Some(trouble) = &list.trouble {
                warn!("the server refused a map request: {trouble}");
            }
            menu.selected = menu.selected.min(list.maps.len().saturating_sub(1));
            menu.known = list;
        }
    }
}

/// Update: F2 opens it, F2 or Escape closes it.
///
/// A function key because every letter belongs to the fields. Opening gives the cursor back and
/// closing does not take it again — a click does that, which is the rule the rest of the game
/// already has.
fn open_and_close(
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<MapMenu>,
    cursor: Option<Single<&mut bevy::window::CursorOptions, With<PrimaryWindow>>>,
) {
    let wanted = if keys.just_pressed(KeyCode::F2) {
        !menu.open
    } else if menu.open && keys.just_pressed(KeyCode::Escape) {
        false
    } else {
        return;
    };
    menu.open = wanted;
    if wanted && let Some(cursor) = cursor {
        let mut cursor = cursor.into_inner();
        cursor.grab_mode = bevy::window::CursorGrabMode::None;
        cursor.visible = true;
    }
}

/// Update: everything typed while the menu is open.
///
/// Reads `KeyboardInput` rather than `ButtonInput<KeyCode>` because it needs the *text* a key
/// produced, which is what makes the fields take a keyboard layout rather than a US one.
fn edit(
    mut typed: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mut menu: ResMut<MapMenu>,
    sender: Option<Single<&mut MessageSender<MapRequest>>>,
) {
    if !menu.open {
        // Anything typed while it was shut belongs to the game, not to a map name.
        typed.clear();
        return;
    }
    let held = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let mut ask: Option<MapRequest> = None;

    for event in typed.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        // Everything that is a *command* comes off the physical key, and only text comes off the
        // logical one. Tab is Tab wherever a layout puts it, and Ctrl+N is under the same finger on
        // every keyboard; what a key means as a letter is the only thing the layout gets to decide.
        // Reading Tab and Backspace off `logical_key` is how the first version of this failed —
        // silently, since a key that matched nothing simply did nothing.
        match event.key_code {
            KeyCode::Tab => {
                let back = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
                let step = if back { FIELDS.len() - 1 } else { 1 };
                menu.focus = (menu.focus + step) % FIELDS.len();
            }
            KeyCode::ArrowUp => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::ArrowDown => {
                let last = menu.known.maps.len().saturating_sub(1);
                menu.selected = (menu.selected + 1).min(last);
            }
            KeyCode::Backspace => {
                let focus = menu.focus;
                menu.values[focus].pop();
            }
            KeyCode::Delete => {
                let focus = menu.focus;
                menu.values[focus].clear();
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                if let Some(name) = menu.known.maps.get(menu.selected).cloned() {
                    ask = Some(MapRequest::Load { name });
                }
            }
            KeyCode::KeyN if held => ask = Some(menu.request_to_create()),
            KeyCode::KeyS if held => {
                let typed = menu.value(Field::Name).trim().to_string();
                let name = if typed.is_empty() {
                    menu.known.current.clone().unwrap_or_default()
                } else {
                    typed
                };
                ask = Some(MapRequest::Save { name });
            }
            _ => {
                if let Key::Character(text) = &event.logical_key
                    && !held
                {
                    let focus = menu.focus;
                    for character in text.chars().filter(|c| !c.is_control()) {
                        menu.values[focus].push(character);
                    }
                }
            }
        }
    }

    if let Some(request) = ask
        && let Some(sender) = sender
    {
        sender.into_inner().send::<TerrainChannel>(request);
    }
}

/// Update: writes the panel out, whenever anything it shows has changed.
fn draw(
    menu: Res<MapMenu>,
    panel: Option<Single<&mut Visibility, With<MenuPanel>>>,
    text: Option<Single<&mut Text, With<MenuText>>>,
) {
    if !menu.is_changed() {
        return;
    }
    if let Some(panel) = panel {
        *panel.into_inner() = if menu.open { Visibility::Visible } else { Visibility::Hidden };
    }
    if menu.open && let Some(text) = text {
        text.into_inner().0 = render(&menu);
    }
}

/// Update: while the menu is up, the game is not being played.
///
/// The keyboard is typing a map name and the pointer is back where it can reach the window, so
/// nothing that happens in here is meant to reach the world. The look angles are left alone —
/// they are the camera's, not the input's, and the camera has stopped turning anyway with the
/// cursor released.
fn take_the_keyboard(
    menu: Res<MapMenu>,
    player: Single<&crate::local_player::LocalPlayer>,
    mut input: ResMut<crate::local_player::CurrentInput>,
) {
    if menu.open {
        input.0 = noob_tube_shared::player::PlayerInput {
            yaw: player.yaw,
            pitch: player.pitch,
            ..Default::default()
        };
    }
}

/// The whole menu as text.
///
/// One text node rather than a tree of them, and deliberately: this is a list and a form, both of
/// which are lines, and a node per line would be more code to say the same thing with no more in
/// it. It becomes a tree the day something in here has to be clicked.
fn render(menu: &MapMenu) -> String {
    let mut out = String::from("MAPS\n\n");

    if menu.known.maps.is_empty() {
        out.push_str("  (no maps yet — make one below)\n");
    }
    for (index, name) in menu.known.maps.iter().enumerate() {
        let cursor = if index == menu.selected { ">" } else { " " };
        let playing = if menu.known.current.as_deref() == Some(name.as_str()) {
            if menu.known.unsaved { "   ← playing, unsaved" } else { "   ← playing" }
        } else {
            ""
        };
        out.push_str(&format!("  {cursor} {name}{playing}\n"));
    }
    if menu.known.current.is_none() {
        out.push_str("\n  Playing the built-in map, which has no file. Ctrl+S keeps it.\n");
    }
    out.push_str("\n  ↑ ↓ to choose, Enter to load. Everybody is moved to it.\n\n");

    out.push_str("NEW MAP\n\n");
    for (index, (field, label)) in FIELDS.iter().enumerate() {
        let caret = if index == menu.focus { "_" } else { "" };
        let unit = if matches!(field, Field::Name) { "" } else { " m" };
        out.push_str(&format!("  {label:<9} {}{caret}{unit}\n", menu.values[index]));
    }

    out.push_str("\n  ");
    match menu.described() {
        Ok(grid) => {
            let samples = grid.samples();
            let bytes = samples * 2;
            out.push_str(&format!(
                "{} × {} samples, {:.0} KB to every joiner",
                grid.nx,
                grid.nz,
                bytes as f32 / 1024.0,
            ));
            if bytes > MAX_BASELINE_BYTES {
                out.push_str(" — over the cap");
            }
        }
        Err(trouble) => out.push_str(&trouble),
    }
    out.push_str("\n\n  Tab moves between fields. Ctrl+N makes it. Ctrl+S saves under Name.\n");

    match sanitise_name(menu.value(Field::Name)) {
        Ok(clean) if clean != menu.value(Field::Name).trim() => {
            out.push_str(&format!("\n  It will be called \"{clean}\".\n"));
        }
        Err(_) if !menu.value(Field::Name).trim().is_empty() => {
            out.push_str("\n  That name has nothing usable in it.\n");
        }
        _ => {}
    }
    if let Some(trouble) = &menu.known.trouble {
        out.push_str(&format!("\n  The server said: {trouble}\n"));
    }
    out.push_str("\n  F2 or Escape closes this.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form has to start in a state that would be accepted, or the first thing anybody does
    /// with it is read an error message. These are the built-in map's own numbers.
    #[test]
    fn the_form_starts_on_a_map_the_server_would_take() {
        let menu = MapMenu::default();
        let grid = menu.described().expect("the defaults describe a map");
        assert_eq!((grid.nx, grid.nz), (513, 513));
    }

    /// And what it refuses, it refuses for the server's reasons rather than its own.
    ///
    /// This is the point of going through `Grid::new` instead of comparing against the caps here:
    /// a second copy of them would drift, and the drift would show up as a map that looks fine in
    /// the dialog and is refused the moment it is asked for.
    #[test]
    fn the_form_is_checked_against_the_rule_the_server_enforces() {
        let mut menu = MapMenu::default();
        menu.values[3] = "0.05".into();
        assert!(menu.described().is_err(), "a spacing under the cap was accepted");

        let mut menu = MapMenu::default();
        menu.values[1] = "10000".into();
        menu.values[2] = "10000".into();
        assert!(menu.described().is_err(), "a ten-kilometre map was accepted");

        let mut menu = MapMenu::default();
        menu.values[4] = "64".into();
        menu.values[5] = "-64".into();
        assert!(menu.described().is_err(), "a map with its floor above its ceiling was accepted");

        let mut menu = MapMenu::default();
        menu.values[1] = "not a number".into();
        assert!(menu.described().is_err(), "a map of unparseable width was accepted");
    }
}
