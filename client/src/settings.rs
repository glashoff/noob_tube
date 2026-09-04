//! What a player may set, and the one description of it everything else reads.
//!
//! **A setting is declared once.** [`Settings`] holds the values; [`Setting`] is the list of them,
//! and every question the dialog asks — what is this called, what may it be, what does it say, what
//! does it cost — is a match arm beside the others. Adding one is a field, a variant, and an arm in
//! each of the five small functions below; the menu, the slider, the keyboard and the mouse all
//! follow without being touched, because none of them knows what a setting *is*.
//!
//! That is the whole reason this is not four constants in `map_menu.rs`. There is one setting
//! today and there will be a page of them, and a page of them written as a page of them is a page
//! of special cases.
//!
//! Nothing here is map content and nothing here travels: these are preferences about *this*
//! machine's picture, and a second player's grass distance is none of this client's business. What
//! is decided here can never change what a player may stand on — see
//! [`grass`](crate::grass), which is a picture with no collider in it.
//!
//! **They are kept in a file, and the file follows the player rather than the directory.** See
//! [`settings_path`] — and [`Place`], which is what a browser has instead, and why it is not the
//! same thing. That is the one thing that separates them from `noob_tube.toml`, which is
//! about a *session* and is read by both binaries and written by neither: these belong to whoever
//! is at this keyboard, and the game writes them itself whenever the dialog changes one. What comes
//! back out of the file goes through the same clamp the slider does, because a file can be edited
//! by hand and can be older than the game reading it.

#[cfg(not(target_family = "wasm"))]
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// How long the settings have to sit still before they are written.
///
/// A slider under the pointer changes the value every frame, and a file written sixty times a
/// second is a file written for nobody. Long enough that a drag across the whole bar is one write,
/// short enough that nothing is lost by quitting straight after letting go.
const WRITE_AFTER: f32 = 0.5;

/// The far end of [`Setting::Sight`]'s range, where it stops being a distance and means "no limit".
///
/// A constant rather than a number written three times, because three things read it and they have
/// to agree about it: the dialog says "off" here, [`Settings::sight`] answers `None` here, and a
/// hand-typed file asking for more is clamped to here. Written as one rule so that moving it moves
/// all three.
///
/// 512 m is [`DEFAULT_EXTENT`](noob_tube_shared::terrain::DEFAULT_EXTENT), the span of the map this
/// game starts on — far enough that a player at one corner sees the other, which is what "no limit"
/// has to mean to be worth the word.
const SIGHT_UNLIMITED: f32 = 512.0;

/// Where the settings are kept when `NOOB_TUBE_SETTINGS` does not say.
///
/// Under the player's config directory rather than beside `noob_tube.toml` in the working
/// directory, and the difference is what the two files are *about*. `noob_tube.toml` describes a
/// session — both binaries read it, neither writes it, and it belongs to the checkout it sits in.
/// These are one person's preferences about their own picture, and following them from one
/// directory to the next is the whole point: a game launched from somewhere else is still their
/// game.
///
/// `NOOB_TUBE_SETTINGS` overrides it, in the same spirit as `NOOB_TUBE_CONFIG` and for a sharper
/// reason: two clients on one machine share this file otherwise, and the second one to write wins.
/// A harness, a bot, or an agent testing beside somebody playing wants its own.
#[cfg(not(target_family = "wasm"))]
pub fn settings_path() -> PathBuf {
    if let Some(named) = std::env::var_os("NOOB_TUBE_SETTINGS") {
        return PathBuf::from(named);
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("noob_tube")
        .join("settings.toml")
}

/// Where the settings are kept: a file on this machine, or a key in this browser's storage.
///
/// The two are the same idea and not the same mechanism, and the difference is not that a browser
/// has no files. It is that a browser tab has nowhere a *player* could go and open one: half of
/// what `settings.toml` is for on a desktop — something you can find, read and edit — has no
/// equivalent there. What is left is the half the game needs, which is that a preference outlives
/// the tab. The text in it is the same TOML either way, so a setting can still be read by whoever
/// goes looking with the developer tools open.
#[cfg(not(target_family = "wasm"))]
type Place = PathBuf;
#[cfg(target_family = "wasm")]
type Place = String;

/// Where this client keeps them.
#[cfg(not(target_family = "wasm"))]
fn place() -> Place {
    settings_path()
}

/// `?settings=` names a different key, for the same reason `NOOB_TUBE_SETTINGS` names a different
/// file: two clients on one machine — here, two tabs on one origin — would otherwise share these,
/// and the second one to write would win.
#[cfg(target_family = "wasm")]
fn place() -> Place {
    crate::platform::setting("NOOB_TUBE_SETTINGS")
        .unwrap_or_else(|| "noob_tube.settings".to_string())
}

/// The place, as a log line wants to name it.
#[cfg(not(target_family = "wasm"))]
fn shown(place: &Place) -> String {
    place.display().to_string()
}

#[cfg(target_family = "wasm")]
fn shown(place: &Place) -> String {
    format!("the browser's {place}")
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        // Read before the app runs rather than in a Startup system, because the first frame already
        // uses them: the lawn is grown from `grass_reach` immediately, and a client that started on
        // the defaults would grow a lawn and throw it away again.
        let place = place();
        let said = read_from(&place);
        app.register_type::<Settings>()
            .insert_resource(said.clone().unwrap_or_default())
            .insert_resource(Kept { place, said })
            .add_systems(Update, keep_the_settings);
    }
}

/// What is already on disk, so that a launch does not write the file it has just read.
///
/// `None` means there was nothing usable there, and the first write will make it — a first run
/// leaves a settings file behind on purpose, so that it is something a player can find and open
/// rather than something they have to be told the path of.
///
/// A file that would not *parse* counts as kept, holding the defaults the game fell back to. It is
/// somebody's own text with a mistake in it, and overwriting it the moment they start the game
/// takes away both the mistake and any chance of seeing what it was. It goes when they change a
/// setting, which is when they have asked for the file to say something else.
#[derive(Resource)]
struct Kept {
    /// Where they are kept. Worked out once, when the plugin is built, rather than on every write:
    /// the answer cannot change while the game runs, and reading the environment sixty times a
    /// second to be told the same thing is sixty chances to be told something else.
    place: Place,
    /// What it already says, or `None` for a file that is not there yet.
    said: Option<Settings>,
}

/// Update: writes the settings once they have stopped changing.
///
/// The delay is on the *change*, not on the difference from the file: a slider under the pointer
/// changes the value every frame, and a deadline pushed forward by every frame that differs from
/// the file is a deadline that never arrives. That is exactly how the first version of this failed
/// — silently, since a file that is never written looks the same as one that cannot be.
fn keep_the_settings(
    time: Res<Time>,
    settings: Res<Settings>,
    mut kept: ResMut<Kept>,
    mut due: Local<Option<f32>>,
) {
    if settings.is_changed() {
        *due = Some(time.elapsed_secs() + WRITE_AFTER);
    }
    let Some(at) = *due else {
        return;
    };
    if time.elapsed_secs() < at {
        return;
    }
    *due = None;
    // A drag that ended where it started, or the frame the resource was inserted on: changed, and
    // with nothing to say that the file does not already say.
    if kept.said.as_ref() == Some(&*settings) {
        return;
    }
    kept.said = Some(settings.clone());
    let place = kept.place.clone();
    match write_to(&place, &settings) {
        // Down at debug: this happens whenever a slider is let go, and a line a player cannot act
        // on is a line in the way of the ones they can.
        Ok(()) => debug!("settings written to {}", shown(&place)),
        // A warning and no more. Nothing here is worth interrupting a game over — the settings are
        // in effect either way, they are simply not kept.
        Err(trouble) => warn!("the settings could not be written to {}: {trouble}", shown(&place)),
    }
}

/// Reads the settings, or `None` when there is no file to read.
///
/// The distinction is [`Kept`]'s: `None` is a first run and a file to be made, and anything else is
/// a file that already exists and is not to be written over until somebody asks for it to be. A
/// file that will not parse is `Some(defaults)` for that reason, and it is worth a word in the log
/// — somebody edited it, and would otherwise watch their changes quietly do nothing.
///
/// Either way the game starts. Nothing in here is worth refusing to play over.
#[cfg(not(target_family = "wasm"))]
fn read_from(path: &Path) -> Option<Settings> {
    Some(parse(&std::fs::read_to_string(path).ok()?, &shown(&path.to_path_buf())))
}

/// The same, out of the browser's storage.
///
/// Storage that is switched off — a private window, a browser set to block site data — reads as no
/// settings rather than as an error, which is the same answer a first run gets and wants the same
/// behaviour: play on the defaults, and write when something changes.
#[cfg(target_family = "wasm")]
fn read_from(key: &str) -> Option<Settings> {
    let text = crate::platform::storage()?.get_item(key).ok().flatten()?;
    Some(parse(&text, &shown(&key.to_string())))
}

/// What was kept, or the defaults and a word about why.
fn parse(text: &str, place: &str) -> Settings {
    match toml::from_str::<Settings>(text) {
        Ok(settings) => settings.tidied(),
        Err(trouble) => {
            warn!("{place} is not settings I can read ({trouble}); using the defaults");
            Settings::default()
        }
    }
}

/// Writes them, making the directory if it is not there yet.
///
/// Written beside and moved into place. A rename is atomic where a write is not: a crash, or a
/// second client writing at the same moment, would otherwise be able to leave half a file where the
/// settings used to be — and the half-file is what the next run would read.
#[cfg(not(target_family = "wasm"))]
fn write_to(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(settings).map_err(std::io::Error::other)?;
    let beside = path.with_extension("toml.new");
    std::fs::write(&beside, text)?;
    std::fs::rename(&beside, path)
}

/// The same, into the browser's storage.
///
/// No write-beside-and-rename here, and none is needed: a `setItem` either happens or does not.
/// What it can be is *refused* — the quota is small and a browser may be keeping nothing at all —
/// so the caller's warning is the whole of the error handling, exactly as it is for a read-only
/// disk.
#[cfg(target_family = "wasm")]
fn write_to(key: &str, settings: &Settings) -> Result<(), String> {
    let storage = crate::platform::storage()
        .ok_or_else(|| "this browser is not keeping site data".to_string())?;
    let text = toml::to_string_pretty(settings).map_err(|trouble| trouble.to_string())?;
    storage
        .set_item(key, &text)
        .map_err(|_| "the browser refused to keep them".to_string())
}

/// Everything a player has set.
///
/// Registered for reflection so it can be read and written over BRP while the game runs, which is
/// how a setting gets tested from outside without a hand on the mouse.
/// `serde(default)` on the whole struct rather than on each field, and that is the difference
/// between a settings file that survives the next version and one that does not: a field this game
/// knows and the file does not mention comes from [`Default`], with the value that setting was
/// designed around, rather than from `f32`'s idea of a default, which is zero and which for
/// `grass_reach` means "off". A field the *file* has and the game does not is ignored, which is
/// what lets a setting be taken away without stranding everyone's file.
#[derive(Resource, Reflect, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[reflect(Resource)]
#[serde(default)]
pub struct Settings {
    /// How far from the eye grass is drawn, in metres. Zero is no grass at all.
    pub grass_reach: f32,
    /// How far from the eye anything is drawn, in metres. See [`Settings::sight`], which is where
    /// the top of the range stops being a distance and starts meaning "no limit".
    pub sight_reach: f32,
}

impl Default for Settings {
    fn default() -> Self {
        // The sight starts switched off. The setting is here to be reached for by somebody whose
        // machine is short of frames, and a game that quietly drew a horizon at 200 m on a first
        // run would be answering a question nobody had asked yet.
        Self { grass_reach: 16.0, sight_reach: SIGHT_UNLIMITED }
    }
}

impl Settings {
    /// Puts every value back inside what its own [`Setting`] allows.
    ///
    /// Through [`Setting::set`], which is the clamp and the rounding the slider already uses. One
    /// rule about what a setting may be — not one for the dialog and a second for the file, which
    /// is how a hand-typed `grass_reach = 5000` becomes a lawn nobody can draw.
    fn tidied(mut self) -> Self {
        for setting in Setting::ALL {
            let value = setting.read(&self);
            setting.set(&mut self, value);
        }
        self
    }

    /// How far the player has asked to see, or `None` for as far as there is anything to see.
    ///
    /// **The top of the slider is not a distance.** It is the setting switched off, and the
    /// difference is one the two readers can feel: the fog is *removed* rather than pushed out to
    /// 512 m, and the camera's far plane goes back to what the projection was built with rather
    /// than being pinned to a number that happens to be large. A map bigger than the default would
    /// otherwise be quietly cropped by a slider sitting at "off".
    ///
    /// It is a method rather than a comparison at each call site because there are three of them —
    /// the two above and [`Setting::say`] — and a rule spelled in three places is a rule with three
    /// chances to be spelled differently.
    pub fn sight(&self) -> Option<f32> {
        (self.sight_reach < SIGHT_UNLIMITED).then_some(self.sight_reach)
    }
}

/// One thing a player can set.
///
/// The order is the order the dialog lists them in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    GrassReach,
    Sight,
}

impl Setting {
    /// Every setting there is. What the dialog builds its rows from.
    pub const ALL: &'static [Setting] = &[Setting::GrassReach, Setting::Sight];

    pub fn label(self) -> &'static str {
        match self {
            Setting::GrassReach => "Grass distance",
            Setting::Sight => "Sight distance",
        }
    }

    /// What it may be: the lowest, the highest, and how far one keypress or one notch moves it.
    ///
    /// The step is part of the setting rather than of the dialog because it is a property of the
    /// quantity: a metre of grass distance is a step somebody can see, and a step nobody can see is
    /// a slider that has to be dragged across the screen to do anything.
    pub fn range(self) -> (f32, f32, f32) {
        match self {
            Setting::GrassReach => (0.0, 64.0, 2.0),
            // The floor is not a taste. Below about sixty metres a 90 degree view is a corridor,
            // and the predecessor's 60 m fog is on record in its own notes as doing balance work
            // rather than graphics work. Sixty-four is that floor on the sixteen-metre step this
            // range is walked in.
            Setting::Sight => (64.0, SIGHT_UNLIMITED, 16.0),
        }
    }

    pub fn read(self, settings: &Settings) -> f32 {
        match self {
            Setting::GrassReach => settings.grass_reach,
            Setting::Sight => settings.sight_reach,
        }
    }

    /// Writes it, held inside its own range and rounded to its own step.
    ///
    /// Both here rather than at the call sites, because there are three of them — the arrow keys,
    /// the wheel and a drag of the pointer — and a rule enforced in three places is a rule with
    /// three chances to be spelled differently.
    pub fn set(self, settings: &mut Settings, value: f32) {
        let (low, high, step) = self.range();
        // A number that is not one cannot be clamped into anything — `f32::clamp` hands NaN
        // straight back — and one can reach here from a hand-edited file or over BRP. What this
        // setting was designed around is the honest answer to a value that is not a value.
        let value = match value.is_finite() {
            true => value,
            false => self.read(&Settings::default()),
        };
        let value = ((value / step).round() * step).clamp(low, high);
        match self {
            Setting::GrassReach => settings.grass_reach = value,
            Setting::Sight => settings.sight_reach = value,
        }
    }

    /// How far along its range it sits, from 0 to 1. What the slider is drawn from.
    pub fn fraction(self, settings: &Settings) -> f32 {
        let (low, high, _) = self.range();
        ((self.read(settings) - low) / (high - low)).clamp(0.0, 1.0)
    }

    /// The value in the fewest words that say it.
    pub fn say(self, settings: &Settings) -> String {
        let value = self.read(settings);
        match self {
            Setting::GrassReach if value <= 0.0 => "off".to_string(),
            Setting::GrassReach => format!("{value:.0} m"),
            // Through `sight` rather than against `SIGHT_UNLIMITED` here, so that what the dialog
            // says and what the horizon does cannot come apart.
            Setting::Sight if settings.sight().is_none() => "off".to_string(),
            Setting::Sight => format!("{value:.0} m"),
        }
    }

    /// What it does and what it costs, for the line under the dialog.
    ///
    /// The cost is in it on purpose. A graphics setting whose price is not stated is one a player
    /// turns up because it is there, and then wonders why the game is slow.
    pub fn note(self) -> &'static str {
        match self {
            Setting::GrassReach => {
                // Plain ASCII, because the dialog's font has no dash of that kind and draws a
                // box for one. Everything on this page goes through the same font.
                "How far grass is drawn. It thins out with distance, so this costs rather less \
                 than the square of it, but it is still the most expensive thing on the page. Off \
                 is off."
            }
            Setting::Sight => {
                "How far you can see. Past it the ground fades into the sky and is then not drawn \
                 at all, which is where the frames come back: this is the one setting here that \
                 makes the whole picture cheaper rather than one thing in it. It can only ever \
                 show you less than off does."
            }
        }
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noob_tube_{name}_{}.toml", std::process::id()))
    }

    /// The top of the sight slider is off, and everything below it is a distance.
    ///
    /// Two things read that rule and they must not come apart: the dialog, which says "off" there,
    /// and `sight`'s horizon, which takes the fog away and gives the far plane back rather than
    /// pinning it to 512 m. The trap is a hand-typed or BRP-set value past the top of the range —
    /// it has to arrive as "off" and not as a far plane nobody chose.
    #[test]
    fn the_top_of_the_sight_slider_is_off_rather_than_far() {
        let mut settings = Settings::default();
        assert_eq!(settings.sight(), None, "the default was not off");
        assert_eq!(Setting::Sight.say(&settings), "off");

        Setting::Sight.set(&mut settings, 128.0);
        assert_eq!(settings.sight(), Some(128.0));
        assert_eq!(Setting::Sight.say(&settings), "128 m");

        Setting::Sight.set(&mut settings, 5000.0);
        assert_eq!(settings.sight(), None, "a value past the range was kept as a distance");
        assert_eq!(Setting::Sight.say(&settings), "off");

        // And under it: the floor is a distance like any other, not a second way to say off.
        Setting::Sight.set(&mut settings, 0.0);
        assert_eq!(settings.sight(), Some(Setting::Sight.range().0));
    }

    /// What is written comes back, which is the whole promise of keeping a file at all.
    #[test]
    fn settings_survive_being_written_and_read() {
        let path = scratch("round_trip");
        let mut settings = Settings::default();
        Setting::GrassReach.set(&mut settings, 34.0);
        write_to(&path, &settings).expect("the settings could not be written");
        assert_eq!(read_from(&path), Some(settings));
        let _ = std::fs::remove_file(&path);
    }

    /// A file the game has never seen, and one it cannot read, both start the game on the defaults.
    ///
    /// The first is every first run there will ever be; the second is somebody who opened the file
    /// and left a bracket behind, and who should get their game rather than a refusal to start.
    #[test]
    fn a_missing_or_broken_file_is_not_a_reason_not_to_play() {
        assert_eq!(read_from(Path::new("/nowhere/at/all/settings.toml")), None, "a missing file was read");

        let path = scratch("broken");
        std::fs::write(&path, "grass_reach = [this is not toml").expect("the scratch file");
        assert_eq!(read_from(&path), Some(Settings::default()));
        let _ = std::fs::remove_file(&path);
    }

    /// A file that says nothing about a setting leaves it at what it was designed around.
    ///
    /// This is what `serde(default)` on the struct buys, and it is the whole of how a settings file
    /// survives a new version: the field added next month is not in anybody's file yet, and the
    /// value it wants is the one in `Default` and not `f32`'s zero — which for the grass would mean
    /// every existing player's lawn silently switching off.
    #[test]
    fn a_file_from_an_older_game_keeps_the_defaults_it_does_not_mention() {
        let path = scratch("older");
        std::fs::write(&path, "# nothing here yet\n").expect("the scratch file");
        assert_eq!(read_from(&path), Some(Settings::default()));
        let _ = std::fs::remove_file(&path);
    }

    /// A hand-typed file cannot ask for a setting the dialog would not have allowed.
    ///
    /// The file is the one way into these values that does not go through a slider, so it is the
    /// one way a lawn could be asked for at five kilometres or at a distance that is not a number.
    /// Both come back inside the range, and by the same rule the slider is held to.
    #[test]
    fn a_hand_written_file_is_held_to_the_same_range_as_the_dialog() {
        let path = scratch("silly");
        let (low, high, _) = Setting::GrassReach.range();

        std::fs::write(&path, "grass_reach = 5000.0\n").expect("the scratch file");
        assert_eq!(Setting::GrassReach.read(&read_from(&path).expect("the scratch file is there")), high, "a lawn to the horizon");

        std::fs::write(&path, "grass_reach = -20.0\n").expect("the scratch file");
        assert_eq!(Setting::GrassReach.read(&read_from(&path).expect("the scratch file is there")), low, "a lawn behind you");

        std::fs::write(&path, "grass_reach = nan\n").expect("the scratch file");
        assert_eq!(read_from(&path), Some(Settings::default()), "a distance that is not a number");
        let _ = std::fs::remove_file(&path);
    }

    /// Changing a setting reaches the file, and only once the changing has stopped.
    ///
    /// An app rather than a pair of calls, because what failed here was neither the reading nor the
    /// writing but the *waiting*: the deadline was pushed forward on every frame whose settings
    /// differed from the file, and a deadline pushed forward every frame never arrives. Nothing was
    /// ever written, and that looks exactly like a file that cannot be written — no error, no line
    /// in the log, no file. The unit tests either side of this one were green throughout.
    #[test]
    fn a_changed_setting_reaches_the_file_once_it_has_settled() {
        use core::time::Duration;

        let path = scratch("settling");
        let _ = std::fs::remove_file(&path);
        let mut app = App::new();
        app.init_resource::<Time>()
            .insert_resource(Settings::default())
            // As a launch that found a file saying exactly the defaults: there is nothing to write
            // until something actually changes.
            .insert_resource(Kept { place: path.clone(), said: Some(Settings::default()) })
            .add_systems(Update, keep_the_settings);

        let tick = |app: &mut App, seconds: f32| {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(Duration::from_secs_f32(seconds));
            app.update();
        };

        tick(&mut app, 0.1);
        assert!(!path.exists(), "a launch that changed nothing wrote the file anyway");

        Setting::GrassReach.set(&mut app.world_mut().resource_mut::<Settings>(), 30.0);
        tick(&mut app, WRITE_AFTER * 0.4);
        assert!(!path.exists(), "the file was written while the slider was still moving");

        // And moving again pushes it out again, which is the whole point of waiting.
        Setting::GrassReach.set(&mut app.world_mut().resource_mut::<Settings>(), 32.0);
        tick(&mut app, WRITE_AFTER * 0.8);
        assert!(!path.exists(), "a second change did not put the write off");

        tick(&mut app, WRITE_AFTER);
        assert_eq!(
            read_from(&path).map(|kept| Setting::GrassReach.read(&kept)),
            Some(32.0),
            "the settling settings never reached the file",
        );

        // Settling on what the file already says writes nothing more.
        let written = std::fs::metadata(&path).expect("the file is there").modified().ok();
        Setting::GrassReach.set(&mut app.world_mut().resource_mut::<Settings>(), 30.0);
        Setting::GrassReach.set(&mut app.world_mut().resource_mut::<Settings>(), 32.0);
        tick(&mut app, WRITE_AFTER * 2.0);
        assert_eq!(std::fs::metadata(&path).expect("still there").modified().ok(), written);
        let _ = std::fs::remove_file(&path);
    }

    /// Where the file lives, and that saying so overrides it.
    ///
    /// The override is what lets two clients share a machine without the second one to write
    /// deciding what the first one's settings are.
    #[test]
    fn the_settings_live_under_the_player_and_can_be_pointed_elsewhere() {
        // SAFETY: single-threaded within this test, and both variables are put back.
        unsafe {
            std::env::set_var("NOOB_TUBE_SETTINGS", "/tmp/somewhere/else.toml");
        }
        assert_eq!(settings_path(), PathBuf::from("/tmp/somewhere/else.toml"));
        unsafe {
            std::env::remove_var("NOOB_TUBE_SETTINGS");
            std::env::set_var("XDG_CONFIG_HOME", "/home/nobody/.config");
        }
        assert_eq!(settings_path(), PathBuf::from("/home/nobody/.config/noob_tube/settings.toml"));
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }
    }
}
