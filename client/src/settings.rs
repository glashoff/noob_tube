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

use bevy::prelude::*;

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Settings>().init_resource::<Settings>();
    }
}

/// Everything a player has set.
///
/// Registered for reflection so it can be read and written over BRP while the game runs, which is
/// how a setting gets tested from outside without a hand on the mouse.
#[derive(Resource, Reflect, Clone, Debug)]
#[reflect(Resource)]
pub struct Settings {
    /// How far from the eye grass is drawn, in metres. Zero is no grass at all.
    pub grass_reach: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { grass_reach: 16.0 }
    }
}

/// One thing a player can set.
///
/// The order is the order the dialog lists them in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    GrassReach,
}

impl Setting {
    /// Every setting there is. What the dialog builds its rows from.
    pub const ALL: &'static [Setting] = &[Setting::GrassReach];

    pub fn label(self) -> &'static str {
        match self {
            Setting::GrassReach => "Grass distance",
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
        }
    }

    pub fn read(self, settings: &Settings) -> f32 {
        match self {
            Setting::GrassReach => settings.grass_reach,
        }
    }

    /// Writes it, held inside its own range and rounded to its own step.
    ///
    /// Both here rather than at the call sites, because there are three of them — the arrow keys,
    /// the wheel and a drag of the pointer — and a rule enforced in three places is a rule with
    /// three chances to be spelled differently.
    pub fn set(self, settings: &mut Settings, value: f32) {
        let (low, high, step) = self.range();
        let value = (value / step).round() * step;
        let value = value.clamp(low, high);
        match self {
            Setting::GrassReach => settings.grass_reach = value,
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
        }
    }
}
