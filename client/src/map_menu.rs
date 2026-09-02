//! The menu: what is in front of the game whenever the game is not being played.
//!
//! **It is up exactly when the pointer is free.** Not a key that opens a panel and another that
//! shuts it — the two states are the same state. If the cursor is yours, you are in the menu; if
//! the game has it, you are playing. That is the whole rule, and everything else follows from it:
//! alt-tabbing away releases the pointer and so raises the menu, "Resume" takes the pointer and so
//! lowers it, and there is no way to be looking at a menu that the game is still listening past.
//!
//! It is a **tree of small dialogs** rather than one page of everything. The root has two entries,
//! and each one that needs details opens a page that asks for exactly those. A page knows its
//! parent, so Escape always means "back" and means it once per level.
//!
//! **Mouse and keyboard both, over one model.** Every page is a list of rows and a selection;
//! hovering moves the selection, clicking activates the row under the pointer, the arrows move the
//! selection and Enter activates it. There is no second code path for the mouse, so there is no
//! way for the two to disagree about what is selected or what a row does.
//!
//! It decides nothing about maps. Every rule about what a map may be lives on the server and in
//! `shared`, which is why the new-map form validates through
//! [`Grid::new`](noob_tube_shared::terrain::Grid::new) rather than against a second copy of the
//! caps. The numbers under the form are the ones those caps are enforced against, and an author
//! should watch them move as they type.

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput};
use bevy::prelude::*;
use bevy::ui::ScrollPosition;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};
use lightyear::prelude::{MessageReceiver, MessageSender, MessageSystems};
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::terrain::{
    Grid, MAX_BASELINE_BYTES, MapList, MapRequest, sanitise_name,
};

/// How the dialog is drawn.
const FONT_SIZE: f32 = 15.0;
const ROW_HEIGHT: f32 = 26.0;
const PADDING: f32 = 20.0;
const DIALOG_WIDTH: f32 = 460.0;
/// How many rows the list shows before it starts scrolling under the selection.
const VISIBLE_ROWS: f32 = 14.0;

pub struct MapMenuPlugin;

impl Plugin for MapMenuPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<MapMenu>()
            .register_type::<Page>()
            .init_resource::<MapMenu>()
            .add_systems(Startup, spawn_dialog)
            // After lightyear has filled the receivers, for the same reason the baseline's reader
            // is: an inbox read before that is always empty.
            .add_systems(PreUpdate, hear_the_server.after(MessageSystems::Receive))
            .add_systems(Update, (follow_the_cursor, operate, rebuild, paint).chain())
            // Immediately after the keyboard is read into the game's input, and never before: the
            // point is to overwrite what was sampled, not to race it.
            .add_systems(
                Update,
                take_the_keyboard.after(crate::local_player::sample_input),
            );
    }
}

/// One page of the menu.
///
/// Each knows its parent, which is what makes Escape mean one thing everywhere. [`Page::Main`] has
/// none, and Escape there means the only thing left: back to the game.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Reflect)]
pub enum Page {
    #[default]
    Main,
    Map,
    New,
    Load,
    SaveAs,
    Delete,
    /// The one page that exists only to be answered. Deleting is the single thing in here that
    /// cannot be undone by doing it again, so it is the single thing that asks twice.
    Confirm,
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Page::Main => "NOOB TUBE",
            Page::Map => "MAP",
            Page::New => "NEW MAP",
            Page::Load => "LOAD MAP",
            Page::SaveAs => "SAVE MAP AS",
            Page::Delete => "DELETE MAP",
            Page::Confirm => "DELETE?",
        }
    }

    fn parent(self) -> Option<Page> {
        match self {
            Page::Main => None,
            Page::Map => Some(Page::Main),
            Page::New | Page::Load | Page::SaveAs | Page::Delete => Some(Page::Map),
            Page::Confirm => Some(Page::Delete),
        }
    }
}

/// One thing that can be typed into.
///
/// Extent and spacing rather than sample counts, because those are what somebody making a map
/// actually thinks in — and the counts are derived and shown back, since they are what the caps
/// are enforced against.
const FIELDS: [&str; 6] = ["Name", "Extent X", "Extent Z", "Spacing", "Lowest", "Highest"];

/// Indices into [`MapMenu::values`]. The first six are [`FIELDS`]; the last is its own, because a
/// name to save under and a name to create under are different questions asked on different pages.
const NAME: usize = 0;
const EXTENT_X: usize = 1;
const EXTENT_Z: usize = 2;
const SPACING: usize = 3;
const LOW: usize = 4;
const HIGH: usize = 5;
const SAVE_AS: usize = 6;

/// What a row does when it is chosen.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Action {
    /// Take the pointer back, which is the same thing as closing the menu.
    Resume,
    Go(Page),
    Back,
    /// Make the map the form describes.
    Create,
    /// Write the map in play under the name it came from — or ask for one, if it has none.
    Save,
    /// Write it under the name in the field.
    SaveUnder,
    /// Take the map named in [`MapMenu::doomed`] off the server.
    DeleteIt,
}

/// One line of a page.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    /// A command.
    Do(Action, &'static str),
    /// An editable field, by its index into [`MapMenu::values`].
    Field(usize, &'static str),
    /// A map to load, by its index into the server's list.
    Map(usize),
}

/// The menu, and everything it is showing.
///
/// Registered for reflection, so what it is showing can be read over BRP while the game runs. That
/// is not decoration: a modal driven by pointer and keyboard has no other way of saying which row
/// the selection is on, and testing it from outside means being able to ask.
#[derive(Resource, Reflect)]
#[reflect(Resource)]
pub struct MapMenu {
    /// Whether the menu is up. Derived from the cursor by [`follow_the_cursor`] and written
    /// nowhere else — the two are one state, and a second source for it would be a second answer.
    pub open: bool,
    pub page: Page,
    /// The last thing the server said. Never edited here — this is a copy of the server's answer,
    /// and the server answers every request, so it is never stale for longer than a round trip.
    known: MapList,
    /// Which row of the current page the selection is on.
    selected: usize,
    /// Every field on every page, so a half-typed name survives a walk through the tree.
    values: Vec<String>,
    /// The map the confirmation page is asking about.
    doomed: String,
    /// Where to go when the server says the last request worked.
    ///
    /// A form should close when what it was filling in has happened, and the client cannot know
    /// that until the answer comes back — so the page to land on is remembered rather than jumped
    /// to. A refusal leaves the form where it is, with the reason under it, which is the only
    /// place it is any use.
    awaiting: Option<Page>,
    /// Set by the click that resumed, cleared when that click is let go.
    ///
    /// The button that takes the pointer back is under the same finger as the trigger, and the
    /// frame after it grabs, a still-held button is indistinguishable from a shot. See
    /// [`take_the_keyboard`].
    swallow: bool,
}

impl Default for MapMenu {
    fn default() -> Self {
        Self {
            open: false,
            page: Page::Main,
            known: MapList::default(),
            selected: 0,
            // The built-in map's own numbers, so the first map somebody makes is a sensible one
            // and the form starts in a state that passes its own caps.
            values: ["", "512", "512", "1", "-64", "64", ""].map(String::from).to_vec(),
            doomed: String::new(),
            awaiting: None,
            swallow: false,
        }
    }
}

impl MapMenu {
    /// The rows of the page being shown, in order.
    ///
    /// Built rather than stored, so there is one description of a page and the drawing, the
    /// keyboard and the pointer all read it. A page whose rows depend on the server's list — the
    /// load page — changes shape when the list does, and this is where that happens.
    fn rows(&self) -> Vec<Row> {
        match self.page {
            Page::Main => vec![
                Row::Do(Action::Resume, "Resume"),
                Row::Do(Action::Go(Page::Map), "Map"),
            ],
            Page::Map => vec![
                Row::Do(Action::Go(Page::New), "New map"),
                Row::Do(Action::Go(Page::Load), "Load map"),
                Row::Do(Action::Save, "Save map"),
                Row::Do(Action::Go(Page::SaveAs), "Save map as"),
                Row::Do(Action::Go(Page::Delete), "Delete map"),
                Row::Do(Action::Back, "Back"),
            ],
            Page::New => FIELDS
                .iter()
                .enumerate()
                .map(|(index, label)| Row::Field(index, label))
                .chain([Row::Do(Action::Create, "Create it"), Row::Do(Action::Back, "Back")])
                .collect(),
            // The same list twice, because picking a map is the same act whatever is about to be
            // done with it. What differs is one line of `activate`, which is where the difference
            // actually is.
            Page::Load | Page::Delete => (0..self.known.maps.len())
                .map(Row::Map)
                .chain([Row::Do(Action::Back, "Back")])
                .collect(),
            Page::SaveAs => vec![
                Row::Field(SAVE_AS, "Name"),
                Row::Do(Action::SaveUnder, "Save it"),
                Row::Do(Action::Back, "Back"),
            ],
            Page::Confirm => vec![
                Row::Do(Action::DeleteIt, "Delete it for good"),
                Row::Do(Action::Back, "Keep it"),
            ],
        }
    }

    /// Takes the server's answer, and lands where the last request was heading.
    ///
    /// A form closes when what it was filling in has happened, and the client cannot know that
    /// until the answer comes back — so a refusal leaves the form where it is, with the reason
    /// under it, which is the only place it is any use. A success also clears the names, which
    /// now belong to a map that exists and would be refused as taken if asked for again.
    fn hear(&mut self, list: MapList) {
        if list.trouble.is_none()
            && let Some(page) = self.awaiting.take()
        {
            self.values[NAME].clear();
            self.values[SAVE_AS].clear();
            self.known = list.clone();
            self.go(page);
        }
        self.known = list;
    }

    fn row(&self, index: usize) -> Option<Row> {
        self.rows().get(index).copied()
    }

    /// Which field the typing goes into, if the selection is on one.
    fn typing_into(&self) -> Option<usize> {
        match self.row(self.selected) {
            Some(Row::Field(index, _)) => Some(index),
            _ => None,
        }
    }

    fn number(&self, index: usize) -> f32 {
        self.values[index].trim().parse().unwrap_or(f32::NAN)
    }

    /// The grid the form describes, or why it is not one.
    ///
    /// Through `Grid::new`, which is the server's own gate. A menu that greyed out over-cap inputs
    /// against its own idea of the caps would be a second copy to drift from the first, and the
    /// drift would show up as a request that looks fine here and is refused there.
    fn described(&self) -> Result<Grid, String> {
        Grid::new(
            self.number(EXTENT_X),
            self.number(EXTENT_Z),
            self.number(SPACING),
            self.number(LOW),
            self.number(HIGH),
        )
        .map_err(|fault| fault.to_string())
    }

    /// Moves the selection, wrapping, and skipping nothing — every row on a page is reachable.
    fn step(&mut self, forward: bool) {
        let count = self.rows().len();
        if count == 0 {
            return;
        }
        let step = if forward { 1 } else { count - 1 };
        self.selected = (self.selected + step) % count;
    }

    /// Opens a page, putting the selection somewhere sensible on it.
    fn go(&mut self, page: Page) {
        self.page = page;
        self.selected = match page {
            // On the map already being played, so Enter on the load page is a reload rather than
            // whatever happens to sort first.
            Page::Load | Page::Delete => self
                .known
                .current
                .as_ref()
                .and_then(|current| self.known.maps.iter().position(|name| name == current))
                .unwrap_or(0),
            // On "keep it". A destructive page that opens with its destructive row under the
            // finger is a page that deletes a map on a stray Enter.
            Page::Confirm => 1,
            _ => 0,
        };
        if page == Page::SaveAs && self.values[SAVE_AS].is_empty() {
            // Prefilled with the name it already has, so "save as" over the same map is a keypress
            // rather than retyping. Only when empty: a name half-typed and navigated away from is
            // still what the author meant.
            self.values[SAVE_AS] = self.known.current.clone().unwrap_or_default();
        }
    }

    /// Does whatever the selected row does. Returns what the server has to be told, if anything.
    fn activate(&mut self) -> Option<MapRequest> {
        match self.row(self.selected)? {
            Row::Field(..) => None,
            Row::Map(index) => {
                let name = self.known.maps.get(index)?.clone();
                if self.page == Page::Delete {
                    self.doomed = name;
                    self.go(Page::Confirm);
                    return None;
                }
                Some(MapRequest::Load { name })
            }
            Row::Do(action, _) => match action {
                // Handled by the caller, which is the only thing holding the cursor.
                Action::Resume => None,
                Action::Go(page) => {
                    self.go(page);
                    None
                }
                Action::Back => {
                    if let Some(parent) = self.page.parent() {
                        self.go(parent);
                    }
                    None
                }
                Action::DeleteIt => {
                    let name = std::mem::take(&mut self.doomed);
                    // Back to the list, which is where a second one would be deleted from, and
                    // which will have this one gone from it by the time the answer lands.
                    self.go(Page::Delete);
                    (!name.is_empty()).then_some(MapRequest::Delete { name })
                }
                Action::Create => Some(MapRequest::Create {
                    name: self.values[NAME].trim().to_string(),
                    extent_x: self.number(EXTENT_X),
                    extent_z: self.number(EXTENT_Z),
                    spacing: self.number(SPACING),
                    min_y: self.number(LOW),
                    max_y: self.number(HIGH),
                })
                .inspect(|_| self.awaiting = Some(Page::Map)),
                // The built-in map has no file behind it, so there is nothing to save *over* and
                // the only honest thing to do is ask for a name.
                Action::Save => match self.known.current.clone() {
                    Some(name) => Some(MapRequest::Save { name }),
                    None => {
                        self.go(Page::SaveAs);
                        None
                    }
                },
                Action::SaveUnder => {
                    let name = self.values[SAVE_AS].trim().to_string();
                    (!name.is_empty())
                        .then_some(MapRequest::Save { name })
                        .inspect(|_| self.awaiting = Some(Page::Map))
                }
            },
        }
    }
}

/// The dialog box.
#[derive(Component)]
struct Dialog;

/// Its heading.
#[derive(Component)]
struct DialogTitle;

/// The line under the heading, saying what is being played.
#[derive(Component)]
struct DialogNote;

/// The box the rows live in, which is also what scrolls.
#[derive(Component)]
struct RowList;

/// One row, by its index into the page.
#[derive(Component)]
struct MenuRow(usize);

/// Everything under the rows: what the form adds up to, and what the server said about it.
#[derive(Component)]
struct DialogFoot;

/// Startup: builds the dialog, if there is a screen to put it on.
///
/// The window query is what leaves it out of a headless client — no window, no system run, and no
/// menu nobody can see. The same guard the HUD uses.
fn spawn_dialog(windows: Query<(), With<PrimaryWindow>>, mut commands: Commands) {
    if windows.is_empty() {
        return;
    }
    commands
        .spawn((
            Name::from("Menu"),
            Dialog,
            Node {
                position_type: PositionType::Absolute,
                left: Val::Percent(50.0),
                top: Val::Percent(50.0),
                width: Val::Px(DIALOG_WIDTH),
                // Centred on the middle of the screen rather than filling it: the world stays
                // visible around the dialog, which is what makes "I am still in a game" legible.
                margin: UiRect {
                    left: Val::Px(-DIALOG_WIDTH / 2.0),
                    top: Val::Px(-160.0),
                    ..default()
                },
                flex_direction: FlexDirection::Column,
                border: UiRect::all(Val::Px(1.0)),
                padding: UiRect::all(Val::Px(PADDING)),
                row_gap: Val::Px(10.0),
                ..default()
            },
            BackgroundColor(Color::srgba(0.05, 0.05, 0.07, 0.94)),
            BorderColor::all(Color::srgba(0.5, 0.55, 0.6, 0.35)),
            Visibility::Hidden,
        ))
        .with_children(|dialog| {
            dialog.spawn((
                DialogTitle,
                Text::new(String::new()),
                TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE), ..default() },
                TextColor(Color::srgb(0.95, 0.8, 0.4)),
            ));
            dialog.spawn((
                DialogNote,
                Text::new(String::new()),
                TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE - 1.0), ..default() },
                TextColor(Color::srgb(0.72, 0.76, 0.8)),
            ));
            dialog.spawn((
                RowList,
                Node {
                    flex_direction: FlexDirection::Column,
                    max_height: Val::Px(ROW_HEIGHT * VISIBLE_ROWS),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
            ));
            dialog.spawn((
                DialogFoot,
                Text::new(String::new()),
                TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE - 2.0), ..default() },
                TextColor(Color::srgb(0.62, 0.66, 0.7)),
            ));
        });
}

/// PreUpdate: takes the server's answer.
///
/// Every request is answered, including the ones that fail, so a menu that asked for something and
/// heard nothing back would be looking at a lost packet — which this channel does not have.
fn hear_the_server(
    mut inbox: Query<&mut MessageReceiver<MapList>>,
    mut menu: ResMut<MapMenu>,
    mut recorder: ResMut<crate::recording::Recorder>,
) {
    for mut receiver in inbox.iter_mut() {
        for list in receiver.receive() {
            if let Some(trouble) = &list.trouble {
                warn!("the server refused a map request: {trouble}");
            }
            // A map switch changes the ground under everything, so it is exactly the kind of event
            // a trace has to carry: without it a recording would show a player falling for no
            // reason anybody reading it could see.
            if list.current != menu.known.current {
                recorder.note(format!("map is now {:?}", list.current));
            }
            menu.hear(list);
        }
    }
}

/// Update: the menu is up exactly while the pointer is free.
///
/// One direction only. Nothing else writes `open`, so there is no state in which the menu believes
/// itself shut while the cursor says otherwise — the failure that a separate open flag invites and
/// that leaves a player looking at a panel the game is still reading the keyboard past.
fn follow_the_cursor(
    cursor: Option<Single<&CursorOptions, With<PrimaryWindow>>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut menu: ResMut<MapMenu>,
) {
    // No window, no menu — and no blanking of a scripted client's input either.
    let up = cursor.is_some_and(|cursor| cursor.grab_mode == CursorGrabMode::None);
    if menu.open != up {
        menu.open = up;
        if !up {
            // Shut at the root, so the next release starts where a main menu starts rather than
            // three pages into a form nobody remembers opening.
            menu.go(Page::Main);
        }
    }
    if menu.swallow && !mouse.pressed(MouseButton::Left) {
        menu.swallow = false;
    }
}

/// Update: everything the pointer and the keyboard do to the menu.
///
/// One system for both, because they act on one model: whatever moved the selection last is what
/// Enter or a click acts on. Splitting them would be two ideas of what is selected.
fn operate(
    mut typed: MessageReader<KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    rows: Query<(&MenuRow, &Interaction)>,
    mut menu: ResMut<MapMenu>,
    cursor: Option<Single<&mut CursorOptions, With<PrimaryWindow>>>,
    sender: Option<Single<&mut MessageSender<MapRequest>>>,
) {
    // One way in and one way out, and it is the same key: Escape gives the pointer up, and a free
    // pointer *is* the menu. There was a second door on F2 for a while and it was one too many —
    // a menu with two openings has two things to remember and a function key nobody could spend
    // on anything else.
    if !menu.open {
        // Anything typed while it was shut belongs to the game, not to a map name.
        typed.clear();
        if keys.just_pressed(KeyCode::Escape)
            && let Some(cursor) = cursor
        {
            let mut cursor = cursor.into_inner();
            cursor.grab_mode = CursorGrabMode::None;
            cursor.visible = true;
            menu.go(Page::Main);
        }
        return;
    }

    let mut ask: Option<MapRequest> = None;
    let mut resume = false;

    // The pointer first, so a click and a keypress in the same frame agree about the row: hovering
    // moves the selection, and Enter and a click then mean the same thing.
    for (row, interaction) in rows.iter() {
        match interaction {
            Interaction::Hovered => menu.selected = row.0,
            Interaction::Pressed => {
                menu.selected = row.0;
                // On the press itself rather than for as long as it is held, or a held button
                // would walk down the tree a page per frame.
                if mouse.just_pressed(MouseButton::Left) {
                    resume |= menu.row(row.0) == Some(Row::Do(Action::Resume, "Resume"));
                    ask = menu.activate().or(ask);
                }
            }
            Interaction::None => {}
        }
    }

    let held = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    for event in typed.read() {
        if event.state != ButtonState::Pressed {
            continue;
        }
        // Everything that is a *command* comes off the physical key, and only text comes off the
        // logical one. Tab is Tab wherever a layout puts it; what a key means as a letter is the
        // only thing the layout gets to decide. Reading Tab and Backspace off `logical_key` is how
        // the first version of this failed — silently, since a key that matched nothing did
        // nothing.
        match event.key_code {
            KeyCode::ArrowDown => menu.step(true),
            KeyCode::ArrowUp => menu.step(false),
            KeyCode::Tab => {
                let back = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
                menu.step(!back);
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                resume |= menu.row(menu.selected) == Some(Row::Do(Action::Resume, "Resume"));
                ask = menu.activate().or(ask);
            }
            KeyCode::Escape => match menu.page.parent() {
                Some(parent) => menu.go(parent),
                // Nowhere left to go back to but the game.
                None => resume = true,
            },
            KeyCode::Backspace => {
                if let Some(field) = menu.typing_into() {
                    menu.values[field].pop();
                }
            }
            KeyCode::Delete => {
                if let Some(field) = menu.typing_into() {
                    menu.values[field].clear();
                }
            }
            _ => {
                if let Key::Character(text) = &event.logical_key
                    && !held
                    && let Some(field) = menu.typing_into()
                {
                    for character in text.chars().filter(|c| !c.is_control()) {
                        menu.values[field].push(character);
                    }
                }
            }
        }
    }

    if resume && let Some(cursor) = cursor {
        let mut cursor = cursor.into_inner();
        cursor.grab_mode = CursorGrabMode::Locked;
        cursor.visible = false;
        // Only a click leaves a button down to be mistaken for a trigger; Escape does not.
        menu.swallow = mouse.pressed(MouseButton::Left);
    }
    if let Some(request) = ask
        && let Some(sender) = sender
    {
        sender.into_inner().send::<TerrainChannel>(request);
    }
}

/// Update: makes the row entities match the page.
///
/// Only when the *shape* changes — a different page, or a list that grew. Selection and typing
/// leave the tree alone and are painted onto it, which is what keeps the pointer's hover from
/// destroying the entity it is hovering over.
fn rebuild(
    menu: Res<MapMenu>,
    list: Option<Single<Entity, With<RowList>>>,
    mut shape: Local<Option<(Page, usize)>>,
    mut commands: Commands,
) {
    let Some(list) = list else { return };
    let rows = menu.rows();
    let now = (menu.page, rows.len());
    if *shape == Some(now) {
        return;
    }
    *shape = Some(now);

    let list = list.into_inner();
    commands.entity(list).despawn_related::<Children>();
    for index in 0..rows.len() {
        commands.entity(list).with_children(|list| {
            list.spawn((
                MenuRow(index),
                Button,
                Node {
                    height: Val::Px(ROW_HEIGHT),
                    width: Val::Percent(100.0),
                    align_items: AlignItems::Center,
                    justify_content: JustifyContent::SpaceBetween,
                    padding: UiRect::horizontal(Val::Px(8.0)),
                    ..default()
                },
                BackgroundColor(Color::NONE),
            ))
            .with_children(|row| {
                for _ in 0..2 {
                    row.spawn((
                        Text::new(String::new()),
                        TextFont { font_size: bevy::text::FontSize::Px(FONT_SIZE), ..default() },
                        TextColor(Color::WHITE),
                    ));
                }
            });
        });
    }
}

/// The three text nodes a page is written into, kept apart so Bevy can hand out three mutable
/// borrows of `Text` at once. Aliases because the disjointness filters are the whole of the type
/// and spelling them inline says nothing a reader wants to read.
type Titles<'w, 's> = Query<
    'w,
    's,
    (&'static mut Text, Has<DialogTitle>, Has<DialogNote>, Has<DialogFoot>),
    Or<(With<DialogTitle>, With<DialogNote>, With<DialogFoot>)>,
>;
type RowTexts<'w, 's> = Query<
    'w,
    's,
    (&'static mut Text, &'static mut TextColor),
    (Without<DialogTitle>, Without<DialogNote>, Without<DialogFoot>),
>;

/// Update: writes the selection, the values and the server's answer onto the rows.
///
/// Every frame the menu is up, rather than on change: the rows it paints may have been spawned by
/// [`rebuild`] this same frame, and there is no change flag on an entity that did not exist yet.
fn paint(
    menu: Res<MapMenu>,
    dialog: Option<Single<&mut Visibility, With<Dialog>>>,
    mut headings: Titles,
    mut list: Query<&mut ScrollPosition, With<RowList>>,
    mut rows: Query<(&MenuRow, &Children, &mut BackgroundColor)>,
    mut texts: RowTexts,
) {
    if let Some(dialog) = dialog {
        *dialog.into_inner() =
            if menu.open { Visibility::Visible } else { Visibility::Hidden };
    }
    if !menu.open {
        return;
    }
    // One query over the three, because they differ only in which string they get, and three
    // `Single`s differing only in their filters was three types nobody could read.
    for (mut text, is_title, is_note, is_foot) in headings.iter_mut() {
        if is_title {
            text.0 = menu.page.title().to_string();
        } else if is_note {
            text.0 = playing(&menu);
        } else if is_foot {
            text.0 = footer(&menu);
        }
    }

    let model = menu.rows();
    for (row, children, mut background) in rows.iter_mut() {
        let Some(what) = model.get(row.0) else { continue };
        let chosen = row.0 == menu.selected;
        background.0 =
            if chosen { Color::srgba(0.35, 0.45, 0.6, 0.55) } else { Color::NONE };
        let (label, value) = describe(&menu, what, chosen);
        for (slot, child) in children.iter().enumerate() {
            let Ok((mut text, mut colour)) = texts.get_mut(child) else { continue };
            match slot {
                0 => {
                    text.0 = label.clone();
                    colour.0 = if chosen {
                        Color::srgb(1.0, 1.0, 1.0)
                    } else {
                        Color::srgb(0.78, 0.8, 0.83)
                    };
                }
                _ => {
                    text.0 = value.clone();
                    colour.0 = Color::srgb(0.6, 0.68, 0.78);
                }
            }
        }
    }

    // Keeps the selection inside the window when the list is longer than the box. Rows are a fixed
    // height on purpose: it is what makes this arithmetic rather than a measurement.
    if let Ok(mut scroll) = list.single_mut() {
        let top = menu.selected as f32 * ROW_HEIGHT;
        let window = ROW_HEIGHT * VISIBLE_ROWS;
        scroll.0.y = scroll.0.y.clamp(top + ROW_HEIGHT - window, top).max(0.0);
    }
}

/// What a row says on the left and on the right.
fn describe(menu: &MapMenu, row: &Row, chosen: bool) -> (String, String) {
    match *row {
        Row::Do(Action::Go(_), label) => (label.to_string(), "›".to_string()),
        Row::Do(_, label) => (label.to_string(), String::new()),
        Row::Field(index, label) => {
            // The caret only where the typing is going, which on a page of six fields is the only
            // thing saying so.
            let caret = if chosen { "_" } else { "" };
            (label.to_string(), format!("{}{caret}", menu.values[index]))
        }
        Row::Map(index) => {
            let name = menu.known.maps.get(index).cloned().unwrap_or_default();
            let playing = menu.known.current.as_ref() == Some(&name);
            let note = match (playing, menu.known.unsaved) {
                (true, true) => "playing, unsaved",
                (true, false) => "playing",
                _ => "",
            };
            (name, note.to_string())
        }
    }
}

/// Everything under the rows.
///
/// What the page needs said, then whatever the server said last. The server's word goes last on
/// every page because it is the only line that is news. What is being *played* is not in here at
/// all — it is under the title, on every page, because it is the one fact every page is about.
fn footer(menu: &MapMenu) -> String {
    let mut out = match menu.page {
        Page::Main => "Arrows to choose, Enter or a click to take it.".to_string(),
        Page::Map => "Saving is what keeps sculpted ground.".to_string(),
        Page::New => {
            let mut out = match menu.described() {
                Ok(grid) => {
                    let bytes = grid.samples() * 2;
                    let mut line = format!(
                        "{} × {} samples, {:.0} KB to every joiner.",
                        grid.nx,
                        grid.nz,
                        bytes as f32 / 1024.0,
                    );
                    if bytes > MAX_BASELINE_BYTES {
                        line.push_str(" Over the cap.");
                    }
                    line
                }
                Err(trouble) => trouble,
            };
            out.push_str("\nTab or the arrows move between fields. Everybody is moved onto it.");
            match sanitise_name(&menu.values[NAME]) {
                Ok(clean) if clean != menu.values[NAME].trim() => {
                    out.push_str(&format!("\nIt will be called \"{clean}\"."));
                }
                Err(_) if !menu.values[NAME].trim().is_empty() => {
                    out.push_str("\nThat name has nothing usable in it.");
                }
                _ => {}
            }
            out
        }
        Page::Load => {
            if menu.known.maps.is_empty() {
                "No maps yet. Make one first.".to_string()
            } else {
                "Loading moves everybody on the server onto it.".to_string()
            }
        }
        Page::SaveAs => {
            let typed = menu.values[SAVE_AS].trim();
            match sanitise_name(typed) {
                _ if typed.is_empty() => "Type a name to save it under.".to_string(),
                Ok(clean) if menu.known.maps.iter().any(|name| name == &clean) => {
                    format!("\"{clean}\" already exists and will be written over.")
                }
                Ok(clean) => format!("It will be saved as \"{clean}\"."),
                Err(trouble) => trouble.to_string(),
            }
        }
        Page::Delete => {
            if menu.known.maps.is_empty() {
                "There is nothing to delete.".to_string()
            } else {
                "Choose one. You will be asked again before it goes.".to_string()
            }
        }
        Page::Confirm => {
            let mut out = format!("\"{}\" will be gone from the server for good.", menu.doomed);
            if menu.known.current.as_ref() == Some(&menu.doomed) {
                out.push_str(
                    "\nYou keep playing it. It stops having a file, which is what unsaved means.",
                );
            }
            out
        }
    };
    if let Some(trouble) = &menu.known.trouble {
        out.push_str(&format!("\nThe server said: {trouble}"));
    }
    out.push_str("\nEsc goes back, and back into the game from here.");
    out
}

/// The line under the title: what is being played, and whether it still matches its file.
///
/// On every page, because every page in here is about it — a load replaces it, a save writes it,
/// a delete takes its file away. A menu that made you go and look would be one that let you save
/// over the wrong map.
fn playing(menu: &MapMenu) -> String {
    match &menu.known.current {
        Some(name) if menu.known.unsaved => format!("Playing {name} — unsaved changes"),
        Some(name) => format!("Playing {name}"),
        None if menu.known.unsaved => {
            "Playing an unsaved map, which has no file behind it".to_string()
        }
        None => "Playing the built-in map, which has no file behind it".to_string(),
    }
}

/// Update: while the menu is up, the game is not being played.
///
/// The keyboard is typing a map name and the pointer is back where it can reach the window, so
/// nothing that happens in here is meant to reach the world. The look angles are left alone — they
/// are the camera's, not the input's, and the camera has stopped turning anyway with the cursor
/// released.
///
/// A scripted client is exempt, and has to be: a harness or a bot runs with the pointer free from
/// the first frame, so blanking on the menu alone would blank every automated run there is.
fn take_the_keyboard(
    menu: Res<MapMenu>,
    scripted: Res<crate::local_player::ScriptedInput>,
    player: Single<&crate::local_player::LocalPlayer>,
    mut input: ResMut<crate::local_player::CurrentInput>,
) {
    if (menu.open || menu.swallow) && scripted.0.is_none() {
        input.0 = noob_tube_shared::player::PlayerInput {
            yaw: player.yaw,
            pitch: player.pitch,
            // Intents are blanked; *modes* are not. Flying is a state the player is in rather than
            // something they are asking for this tick, and dropping it here meant opening the menu
            // in mid-air handed you back to gravity. Blanked and still flying is a hover, which is
            // exactly what a menu over a hillside should be.
            flying: player.flying,
            ..Default::default()
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An app with the menu's own systems and a window to hang a cursor on.
    ///
    /// The plugin is not used: it orders itself against systems from the rest of the client, and
    /// what is under test here is the four systems and the one rule tying them to the pointer.
    fn menu_app() -> App {
        let mut app = App::new();
        app.init_resource::<MapMenu>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .add_message::<KeyboardInput>()
            .add_systems(Startup, spawn_dialog)
            .add_systems(Update, (follow_the_cursor, operate, rebuild, paint).chain());
        app.world_mut().spawn((PrimaryWindow, CursorOptions::default()));
        app
    }

    fn grab_mode(app: &mut App) -> CursorGrabMode {
        app.world_mut()
            .query_filtered::<&CursorOptions, With<PrimaryWindow>>()
            .single(app.world())
            .expect("the test window has a cursor")
            .grab_mode
    }

    fn set_grab(app: &mut App, mode: CursorGrabMode) {
        let mut cursors = app.world_mut().query_filtered::<&mut CursorOptions, With<PrimaryWindow>>();
        cursors.single_mut(app.world_mut()).expect("the test window has a cursor").grab_mode = mode;
    }

    fn on_screen(app: &mut App) -> Visibility {
        *app.world_mut()
            .query_filtered::<&Visibility, With<Dialog>>()
            .single(app.world())
            .expect("the dialog was never spawned")
    }

    /// Presses a key both ways it is read: as a button, which is how the game reads it while the
    /// menu is shut, and as a keyboard event, which is how the menu reads it while it is up. In a
    /// real client winit produces both from one keypress; here they have to be said twice.
    fn tap(app: &mut App, key: KeyCode) {
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().press(key);
        let window = app
            .world_mut()
            .query_filtered::<Entity, With<PrimaryWindow>>()
            .single(app.world())
            .expect("the test window exists");
        app.world_mut().write_message(KeyboardInput {
            key_code: key,
            logical_key: Key::Unidentified(bevy::input::keyboard::NativeKey::Unidentified),
            state: ButtonState::Pressed,
            text: None,
            repeat: false,
            window,
        });
        app.update();
        // No `InputPlugin` here to do it, and a key left pressed would go on being *just* pressed.
        app.world_mut().resource_mut::<ButtonInput<KeyCode>>().clear();
    }

    /// The one rule the whole menu rests on, and the one that cannot be checked by reading: a free
    /// pointer and a menu on screen are the same state.
    ///
    /// Worth an app rather than a unit test on the model, because what would break it is a system
    /// that does not run — a `Single` that matches nothing, an ordering that paints before the
    /// cursor is read — and none of that is visible in the functions themselves.
    #[test]
    fn the_menu_is_up_exactly_when_the_pointer_is_free() {
        let mut app = menu_app();
        app.update();
        assert!(app.world().resource::<MapMenu>().open, "the menu was not up with a free pointer");
        assert_eq!(on_screen(&mut app), Visibility::Visible);

        set_grab(&mut app, CursorGrabMode::Locked);
        app.update();
        assert!(!app.world().resource::<MapMenu>().open, "the menu stayed up with the pointer taken");
        assert_eq!(on_screen(&mut app), Visibility::Hidden);
    }

    /// Escape is the way out of the game and the way out of the menu, and it is the same key both
    /// ways round. Playing, it gives the pointer back; at the root, it takes it again.
    #[test]
    fn escape_goes_both_ways() {
        let mut app = menu_app();
        app.update();

        // Into the game, the way Resume does it.
        set_grab(&mut app, CursorGrabMode::Locked);
        app.update();

        tap(&mut app, KeyCode::Escape);
        assert_eq!(grab_mode(&mut app), CursorGrabMode::None, "escape did not give the pointer back");
        app.update();
        assert!(app.world().resource::<MapMenu>().open, "the menu did not come back with the pointer");
        assert_eq!(app.world().resource::<MapMenu>().page, Page::Main, "escape opened a sub-page");

        tap(&mut app, KeyCode::Escape);
        assert_eq!(grab_mode(&mut app), CursorGrabMode::Locked, "escape at the root did not resume");
    }


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
        menu.values[SPACING] = "0.05".into();
        assert!(menu.described().is_err(), "a spacing under the cap was accepted");

        let mut menu = MapMenu::default();
        menu.values[EXTENT_X] = "10000".into();
        menu.values[EXTENT_Z] = "10000".into();
        assert!(menu.described().is_err(), "a ten-kilometre map was accepted");

        let mut menu = MapMenu::default();
        menu.values[LOW] = "64".into();
        menu.values[HIGH] = "-64".into();
        assert!(menu.described().is_err(), "a map with its floor above its ceiling was accepted");

        let mut menu = MapMenu::default();
        menu.values[EXTENT_X] = "not a number".into();
        assert!(menu.described().is_err(), "a map of unparseable width was accepted");
    }

    /// Every page has a way out, and it is the same key on all of them.
    ///
    /// The one structural claim the tree makes. A page whose parent were itself would trap a
    /// player behind a dialog with the pointer released and the game unreachable.
    #[test]
    fn every_page_leads_back_to_the_root() {
        for page in
            [Page::Main, Page::Map, Page::New, Page::Load, Page::SaveAs, Page::Delete, Page::Confirm]
        {
            let mut at = page;
            for _ in 0..8 {
                match at.parent() {
                    Some(parent) => at = parent,
                    None => break,
                }
            }
            assert_eq!(at, Page::Main, "{page:?} does not lead back to the root");
        }
    }

    /// Walking the tree with the keyboard reaches every dialog, and each one asks its own question.
    #[test]
    fn the_tree_opens_the_dialog_each_entry_promises() {
        let mut menu = MapMenu::default();
        menu.known.maps = vec!["ridge".to_string(), "valley".to_string()];
        menu.known.current = Some("valley".to_string());

        // Main: resume first, map second.
        assert_eq!(menu.rows().len(), 2);
        menu.step(true);
        assert_eq!(menu.activate(), None, "opening the map page asked the server for something");
        assert_eq!(menu.page, Page::Map);

        // New map: six fields to fill in, and creating sends the form.
        menu.go(Page::New);
        assert_eq!(menu.rows().len(), FIELDS.len() + 2);
        menu.values[NAME] = "ridge two".into();
        menu.selected = FIELDS.len();
        let asked = menu.activate().expect("creating asked for nothing");
        assert!(matches!(asked, MapRequest::Create { .. }));

        // Load: one row per map, and the selection starts on the one being played.
        menu.go(Page::Load);
        assert_eq!(menu.rows().len(), 3);
        assert_eq!(menu.selected, 1, "the load page did not start on the map in play");
        assert_eq!(menu.activate(), Some(MapRequest::Load { name: "valley".into() }));

        // Save as: prefilled with the name in play, because saving over it is the common case.
        menu.values[SAVE_AS].clear();
        menu.go(Page::SaveAs);
        assert_eq!(menu.values[SAVE_AS], "valley");
        menu.selected = 1;
        assert_eq!(menu.activate(), Some(MapRequest::Save { name: "valley".into() }));
    }

    /// Saving a map that has no file asks for a name instead of writing one under a guess.
    #[test]
    fn saving_the_built_in_map_asks_what_to_call_it() {
        let mut menu = MapMenu::default();
        menu.go(Page::Map);
        menu.selected = 2;
        assert_eq!(menu.activate(), None, "the built-in map was saved under no name at all");
        assert_eq!(menu.page, Page::SaveAs);

        // And with a name, that same entry writes it without asking anything.
        let mut menu = MapMenu::default();
        menu.known.current = Some("ridge".to_string());
        menu.go(Page::Map);
        menu.selected = 2;
        assert_eq!(menu.activate(), Some(MapRequest::Save { name: "ridge".into() }));
    }

    /// Opening the menu blanks what a player is asking for, and not what they are.
    ///
    /// The distinction cost a fall out of the sky: flight is a mode, and a mode cleared along with
    /// the movement keys handed a flying player back to gravity the moment they pressed Escape.
    #[test]
    fn the_menu_takes_the_keys_and_leaves_the_mode() {
        use crate::local_player::{CurrentInput, LocalPlayer, ScriptedInput};
        use bevy::ecs::system::RunSystemOnce;
        use noob_tube_shared::player::PlayerInput;

        let mut app = menu_app();
        app.init_resource::<CurrentInput>().init_resource::<ScriptedInput>();
        app.world_mut().spawn(LocalPlayer { flying: true, ..default() });
        app.world_mut().resource_mut::<CurrentInput>().0 =
            PlayerInput { forward: true, jump: true, flying: true, ..default() };
        app.update();

        app.world_mut().run_system_once(take_the_keyboard).expect("the keyboard is taken");
        let input = app.world().resource::<CurrentInput>().0;
        assert!(!input.forward, "the menu let a movement key through");
        assert!(!input.jump, "the menu let a movement key through");
        assert!(input.flying, "opening the menu stopped a player flying");
    }

    /// Deleting asks twice, and opens the second question on "keep it".
    ///
    /// The one irreversible thing in the menu, so it is the one thing that does not happen on a
    /// single Enter — and the row under the finger when it opens is the one that does nothing.
    #[test]
    fn deleting_asks_again_before_it_does_anything() {
        let mut menu = MapMenu::default();
        menu.known.maps = vec!["ridge".to_string(), "valley".to_string()];

        menu.go(Page::Delete);
        menu.selected = 1;
        assert_eq!(menu.activate(), None, "picking a map to delete deleted it");
        assert_eq!(menu.page, Page::Confirm);
        assert_eq!(menu.doomed, "valley");
        assert_eq!(menu.selected, 1, "the confirmation opened on the destructive row");

        // "Keep it" is the row it opens on, and it goes back without asking for anything.
        assert_eq!(menu.activate(), None);
        assert_eq!(menu.page, Page::Delete);

        menu.selected = 1;
        menu.activate();
        menu.selected = 0;
        assert_eq!(menu.activate(), Some(MapRequest::Delete { name: "valley".into() }));
        assert_eq!(menu.page, Page::Delete, "deleting did not go back to the list");
    }

    /// A form closes when the server says the thing happened, and not a moment before.
    #[test]
    fn a_created_map_takes_the_menu_with_it() {
        let mut menu = MapMenu::default();
        menu.go(Page::New);
        menu.values[NAME] = "ridge".into();
        menu.selected = FIELDS.len();
        menu.activate().expect("creating asked for nothing");
        assert_eq!(menu.page, Page::New, "the form closed before the server answered");

        // A refusal leaves it open, with the name still in it to be corrected.
        menu.hear(MapList { trouble: Some("that name is taken".into()), ..default() });
        assert_eq!(menu.page, Page::New);
        assert_eq!(menu.values[NAME], "ridge");

        menu.selected = FIELDS.len();
        menu.activate().expect("creating asked for nothing");
        menu.hear(MapList {
            maps: vec!["ridge".into()],
            current: Some("ridge".into()),
            ..default()
        });
        assert_eq!(menu.page, Page::Map, "the form stayed open over a map that exists");
        assert!(menu.values[NAME].is_empty(), "the name of a map that now exists was kept");
    }

    /// Typing goes into a field only while a field is what is selected.
    ///
    /// The rule that lets one page hold both a form and its buttons: what a letter key does
    /// depends on the selection and on nothing else.
    #[test]
    fn typing_lands_in_a_field_and_nowhere_else() {
        let mut menu = MapMenu::default();
        menu.go(Page::New);
        assert_eq!(menu.typing_into(), Some(NAME));
        menu.selected = FIELDS.len();
        assert_eq!(menu.typing_into(), None, "a command row took typing");
        menu.go(Page::Main);
        assert_eq!(menu.typing_into(), None, "the root page took typing");
    }
}
