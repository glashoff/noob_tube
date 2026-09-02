//! The ground: a height field, what it is allowed to be, and the two files it lives in.
//!
//! See `terrain.md` for the design this implements. This is step one of it — the data model and
//! the codec, and deliberately nothing else. There is no rendering here, no collider, and no
//! replication; those are steps two, four and five, and each of them is easier to get right
//! against a model that already round-trips.
//!
//! Three things carry their reasons rather than their values, because the values are arbitrary and
//! the reasons are not.
//!
//! **The quantisation is not a storage trick.** `u16` over an explicit `[min_y, max_y]` range costs
//! half of `f32` and gives 2 mm over a 128 m range, which is more than terrain ever needs. But the
//! reason it is `u16` in memory rather than `f32` rounded on the way to disk is determinism: a
//! sculpt brush rounds to *this* lattice inside the stroke, so every machine lands on the same
//! numbers after every stroke and repeated small strokes cannot accumulate apart. That is what
//! makes it safe to replicate a sculpt as a gesture instead of as a list of samples.
//!
//! **The `+ 1` in the sample count is not an off-by-one.** Heights are grid *points*, not cells, so
//! an n-metre terrain at 1 m spacing needs n+1 of them. It is also why heightmap tools export
//! 2ⁿ+1 sizes — 513, 1025, 2049 — and why a cap written as a round 512² would reject a canonical
//! 513² import by exactly one sample per axis, for no reason at all.
//!
//! **The caps are mandatory, not tidy.** Extent and spacing arrive from a client, and their
//! quotient drives a server-side allocation and the size of every future baseline. Unclamped, that
//! is a denial of service with a one-line exploit: a 10 km map at 5 cm spacing is 40000² samples
//! and 3.2 GB. They live here so the menu can grey out an over-cap input against the same
//! constants the server enforces, rather than against a second copy that drifts.

use avian3d::parry::shape::SharedShape;
use avian3d::parry::utils::Array2;
use avian3d::prelude::*;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// The format this module writes. Bumped when a change would stop an older map loading.
pub const VERSION: u32 = 1;

/// Metres between samples: the finest and coarsest a map may be.
///
/// Finer than a quarter metre buys nothing a sculpt brush can express; coarser than eight is
/// unusable as ground.
pub const MIN_SPACING: f32 = 0.25;
pub const MAX_SPACING: f32 = 8.0;

/// The most samples one axis may have.
///
/// 2049 rather than 2048, so the canonical heightmap sizes land exactly on it — see the note about
/// the `+ 1` above. On its own this is not the guard that matters: two axes each just under it
/// multiply to four million samples, which is why [`MAX_SAMPLES`] exists as well.
pub const MAX_SAMPLES_PER_AXIS: u32 = 2049;

/// What a baseline may cost a joining client, in bytes of heights.
///
/// Sized from the wire rather than from memory, because that is the limit that bites first: the
/// heights are sent to every client that joins, and a map nobody can join is worse than a map that
/// is too small. Four mebibytes admits 1025² — a kilometre at 1 m spacing — and refuses 2049².
pub const MAX_BASELINE_BYTES: usize = 4 << 20;

/// The most samples a map may have in total, from [`MAX_BASELINE_BYTES`].
pub const MAX_SAMPLES: u32 = (MAX_BASELINE_BYTES / core::mem::size_of::<u16>()) as u32;

/// The map a level has until maps can be made and loaded, in step six.
///
/// 512 m at 1 m spacing, which covers the playable area the ground plane it replaces had and lands
/// on 513² — the size heightmap tools export, so an imported field will not need resampling. The
/// range is the one the quantisation was costed against: 128 m, resolved to 2 mm, and deeper than
/// anyone walks out of.
pub const DEFAULT_EXTENT: f32 = 512.0;
pub const DEFAULT_SPACING: f32 = 1.0;
pub const DEFAULT_MIN_Y: f32 = -64.0;
pub const DEFAULT_MAX_Y: f32 = 64.0;

/// How far around the origin the ground is left exactly flat, and where the shaping reaches full
/// strength.
///
/// The spawn points, both vehicle starts, the ramp and every crate stand inside the first radius,
/// on the plane they were placed on. Shaping the ground under them would either bury them or leave
/// them hanging, and neither is a thing to discover by walking into it — the furthest of them is
/// the ramp at 24 m, so 40 leaves room for the next one.
const HOME_FLAT: f32 = 40.0;
const HOME_BLEND: f32 = 90.0;

/// A rounded hill, or a hollow when the height is negative.
struct Swell {
    at: Vec2,
    radius: f32,
    height: f32,
}

/// A ravine, cut along a line rather than around a point.
struct Ravine {
    from: Vec2,
    to: Vec2,
    width: f32,
    depth: f32,
}

/// Somewhere to walk that is not a table.
///
/// Placed by hand rather than generated: four hills and two ravines are enough to tell whether the
/// ground, the slope limit and the camera behave, and a handful of numbers can be moved when they
/// turn out to be in the wrong place. Noise comes later, if at all.
const HILLS: [Swell; 5] = [
    Swell { at: Vec2::new(-140.0, 60.0), radius: 110.0, height: 22.0 },
    Swell { at: Vec2::new(170.0, -120.0), radius: 130.0, height: 28.0 },
    Swell { at: Vec2::new(60.0, 180.0), radius: 90.0, height: 16.0 },
    Swell { at: Vec2::new(-190.0, -150.0), radius: 140.0, height: 34.0 },
    // A bowl, so that "down" is reachable without walking to a ravine.
    Swell { at: Vec2::new(150.0, 130.0), radius: 100.0, height: -20.0 },
];

const RAVINES: [Ravine; 2] = [
    Ravine { from: Vec2::new(-60.0, -70.0), to: Vec2::new(-230.0, 40.0), width: 22.0, depth: 18.0 },
    Ravine { from: Vec2::new(120.0, 40.0), to: Vec2::new(30.0, 210.0), width: 18.0, depth: 14.0 },
];

/// Smoothstep, the polynomial one, on a value already clamped to 0..1.
///
/// Every shaping function here is a polynomial in the *squared* distance and never takes a square
/// root, which is the rule §6 sets for brushes and applies just as much to this: `sqrt`, `powf`,
/// `sin` and `hypot` go through `libm`, which is not required to be correctly rounded and may
/// differ in the last ulp between platforms. Client and server both generate this map until step
/// five sends it, so they have to agree on it exactly.
pub(crate) fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// How much of a feature applies at a squared distance from it: 1 at the middle, 0 at the rim, and
/// flat at both ends.
pub(crate) fn falloff(distance_squared: f32, radius: f32) -> f32 {
    let t = distance_squared / (radius * radius);
    if t >= 1.0 { 0.0 } else { smoothstep(1.0 - t) }
}

/// The squared distance from a point to a segment. No square root anywhere in it.
pub(crate) fn to_segment_squared(point: Vec2, from: Vec2, to: Vec2) -> f32 {
    let along = to - from;
    let length_squared = along.length_squared();
    let t = if length_squared <= 0.0 {
        0.0
    } else {
        ((point - from).dot(along) / length_squared).clamp(0.0, 1.0)
    };
    (point - (from + along * t)).length_squared()
}

/// That map: hills and ravines, and flat where the level already stands.
pub fn default_terrain() -> Terrain {
    let mut terrain =
        Terrain::new(DEFAULT_EXTENT, DEFAULT_EXTENT, DEFAULT_SPACING, DEFAULT_MIN_Y, DEFAULT_MAX_Y)
            .expect("the default map is within its own caps");
    let grid = terrain.grid;
    let flat = HOME_FLAT * HOME_FLAT;
    let blend = HOME_BLEND * HOME_BLEND;
    for iz in 0..grid.nz {
        for ix in 0..grid.nx {
            let here = grid.world_of(ix, iz);
            let mut y = 0.0;
            for hill in &HILLS {
                y += hill.height * falloff((here - hill.at).length_squared(), hill.radius);
            }
            for ravine in &RAVINES {
                let across = to_segment_squared(here, ravine.from, ravine.to);
                y -= ravine.depth * falloff(across, ravine.width);
            }
            // Held down to nothing over the level everything already stands on.
            let home = ((here.length_squared() - flat) / (blend - flat)).clamp(0.0, 1.0);
            y *= smoothstep(home);
            let index = grid.index(ix, iz);
            terrain.heights[index] = grid.quantise(y);
        }
    }
    terrain
}

/// Why a map was refused.
///
/// One enum for creation and for loading, because they reject the same things for the same
/// reasons and a client that is told "too many samples" should not care which path said it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapFault {
    /// Spacing outside [`MIN_SPACING`]..=[`MAX_SPACING`], or not a number.
    Spacing,
    /// An extent that is not a positive, finite number of metres.
    Extent,
    /// More than [`MAX_SAMPLES_PER_AXIS`] along one axis.
    TooLongAnAxis,
    /// More than [`MAX_SAMPLES`] samples in total.
    TooManySamples,
    /// `min_y` and `max_y` do not describe a positive, finite range.
    Range,
    /// The blob of heights is not `nx * nz * 2` bytes.
    ///
    /// The manifest states the sample counts and the blob is however many bytes it is; the two
    /// files can be separated, edited, or half-written. Unchecked, a mismatch reads the height
    /// field off the end of itself.
    BlobSize { expected: usize, found: usize },
    /// A version this code does not know how to read.
    Version(u32),
    /// A map name with nothing usable left in it once it had been sanitised.
    Name,
    /// A name that is not one of the maps the server has.
    ///
    /// The same answer for a map that was never there and for one somebody is trying to reach
    /// out of the directory with: a load resolves a name *against the list*, so there is no
    /// second code path where the traversal check could be forgotten.
    NoSuchMap,
    /// A name that is already taken.
    NameTaken,
    /// Asking for maps faster than [`CREATE_INTERVAL`].
    TooFast,
    /// A stroke with a number in it that is not one, or one past a cap.
    Stroke,
    /// Sculpting faster than [`SAMPLES_PER_SECOND`](crate::sculpt::SAMPLES_PER_SECOND) allows.
    TooMuchGround,
    /// A marker giving both a `yaw` and a `rotation`.
    ///
    /// Refused rather than resolved by precedence. A precedence rule is a thing somebody has to
    /// remember correctly at two in the morning, and the file that needs it is the one nobody
    /// looks at again.
    TwoRotations,
    /// A marker with a number in it that is not one, or one outside what a map can hold.
    Marker,
    /// More markers on one map than [`MAX_MARKERS`].
    TooManyMarkers,
}

impl core::fmt::Display for MapFault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Spacing => {
                write!(f, "spacing must be between {MIN_SPACING} m and {MAX_SPACING} m")
            }
            Self::Extent => write!(f, "the extent must be a positive number of metres"),
            Self::TooLongAnAxis => {
                write!(f, "an axis may have at most {MAX_SAMPLES_PER_AXIS} samples")
            }
            Self::TooManySamples => write!(f, "a map may have at most {MAX_SAMPLES} samples"),
            Self::Range => write!(f, "max_y must be above min_y"),
            Self::BlobSize { expected, found } => {
                write!(f, "the heights are {found} bytes where the manifest says {expected}")
            }
            Self::Version(version) => write!(f, "map version {version} is not one this build reads"),
            Self::Name => write!(f, "a map name needs letters or digits in it"),
            Self::NoSuchMap => write!(f, "there is no map by that name"),
            Self::NameTaken => write!(f, "there is already a map by that name"),
            Self::TooFast => {
                write!(f, "wait {} s between new maps", CREATE_INTERVAL.as_secs())
            }
            Self::TwoRotations => {
                write!(f, "a marker gives either a yaw or a rotation, never both")
            }
            Self::Marker => write!(f, "that is not a place on this map a marker can stand"),
            Self::TooManyMarkers => write!(f, "a map may hold at most {MAX_MARKERS} markers"),
            Self::Stroke => write!(f, "that is not a brush stroke this map will take"),
            Self::TooMuchGround => write!(f, "sculpting faster than the server will take it"),
        }
    }
}

/// The longest a map name may be, in characters.
///
/// Not a technical limit — it is what fits in the menu's list without the eye having to work — but
/// it is enforced on the server all the same, because a name is a file name and a client picks it.
pub const MAX_NAME: usize = 40;

/// How long a client must wait between creating maps.
///
/// Creating is the one action that writes a file that stays written, so it is the one that needs a
/// rate of its own. Loading and saving are limited by the same thing that limits everything else —
/// somebody has to be holding the menu open.
pub const CREATE_INTERVAL: core::time::Duration = core::time::Duration::from_secs(5);

/// What a client's map name is allowed to become.
///
/// Letters, digits, spaces, hyphens and underscores, trimmed, collapsed and capped. Everything else
/// is dropped rather than rejected, so a name with a stray character in it still works — the point
/// is a usable file name, not a lecture.
///
/// This is where `..`, `/` and every other path is disposed of, and it is deliberately an
/// *allowlist*: a denylist of dangerous characters is a list somebody has to keep complete, and the
/// set of things a map may be called is small and known. It is in `shared` so the menu can refuse a
/// name against the same rule the server enforces rather than a second copy that drifts.
///
/// It is still not the guard that matters for loading. That one is [`MapFault::NoSuchMap`]: a load
/// resolves a name against the list of maps the server itself found, so even a name this let
/// through cannot reach a file that is not a map.
pub fn sanitise_name(raw: &str) -> Result<String, MapFault> {
    let mut name = String::with_capacity(raw.len().min(MAX_NAME));
    let mut spaced = false;
    for character in raw.chars() {
        let keep = match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => character,
            ' ' | '\t' => ' ',
            _ => continue,
        };
        // One space at a time, and none at the front.
        if keep == ' ' {
            spaced = !name.is_empty();
            continue;
        }
        if spaced && name.len() < MAX_NAME {
            name.push(' ');
        }
        spaced = false;
        if name.len() >= MAX_NAME {
            break;
        }
        name.push(keep);
    }
    // A name of nothing but punctuation comes out empty, and a file called "" is not a map.
    if name.chars().any(|c| c.is_ascii_alphanumeric()) {
        Ok(name)
    } else {
        Err(MapFault::Name)
    }
}

/// What a client asks the server to do with maps.
///
/// One message rather than four, because they are one conversation and the reply to all of them is
/// the same [`MapList`]. Every one of them is something any connected player may do — there is no
/// ownership here and no permissions, which is the stance the rest of the game takes; the
/// protections are against accident and abuse of *size*, not against the player.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum MapRequest {
    /// Make a new map and put everybody on it. Extent and spacing, never sample counts — those are
    /// derived, and they are what the caps are enforced against.
    Create {
        name: String,
        extent_x: f32,
        extent_z: f32,
        spacing: f32,
        min_y: f32,
        max_y: f32,
    },
    /// Switch everybody to a map that already exists.
    Load { name: String },
    /// Write the map as it currently stands, under this name.
    Save { name: String },
    /// Take a map off the server for good.
    ///
    /// The map *in play* is not touched by this, even when it is the one being deleted: what goes
    /// is the file, and what is left is a game still standing on the ground that came out of it,
    /// now unnamed and unsaved. Ending a round because somebody tidied up a directory would be a
    /// strange thing for a menu to do.
    Delete { name: String },
    /// Just tell me what there is.
    List,
}

/// What the server says back: what maps there are, which one this is, and what went wrong.
///
/// Sent on join and after every [`MapRequest`], so a client never has to ask twice and never has to
/// guess whether its request worked. The trouble travels with the list rather than as a message of
/// its own, because the two are always looked at together — "that name is taken, and here is what
/// is taken" is one answer.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, Reflect)]
pub struct MapList {
    /// Every map on the server, sorted. This is also the only list a load may resolve against.
    pub maps: Vec<String>,
    /// The map being played, or `None` for the built-in one, which has no file behind it.
    pub current: Option<String>,
    /// Whether the map in play differs from the file it came from.
    ///
    /// Nothing sets this yet: until sculpting arrives in step seven the map in play is always
    /// exactly what was created or loaded. It is here now because the menu is the only place that
    /// can say so, and a sculpt that exists only in memory is one disconnect from gone.
    pub unsaved: bool,
    /// What went wrong with the last request, in words a player can read.
    pub trouble: Option<String>,
}

/// The lattice the heights sit on: where the samples are, and what a sample means.
///
/// Everything about a map that is not a height. Copied freely — it is seven numbers, and passing it
/// by value is what lets the quantisation live on it rather than on the terrain, so a brush can
/// round to the lattice without holding the field.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Reflect)]
pub struct Grid {
    /// Samples along x and z. Independent, so a map can be a strip rather than a square.
    pub nx: u32,
    pub nz: u32,
    /// Metres between samples.
    pub spacing: f32,
    /// World x and z of sample (0, 0).
    pub origin_x: f32,
    pub origin_z: f32,
    /// The world heights that sample values 0 and [`u16::MAX`] mean.
    ///
    /// Fixed at creation: changing either would requantise every height in the map, which is a
    /// migration rather than an edit.
    pub min_y: f32,
    pub max_y: f32,
}

impl Grid {
    /// The grid an author's two numbers describe.
    ///
    /// Extent and spacing rather than sample counts, because those are what somebody making a map
    /// actually thinks in. The counts are derived and shown back to them, since they are what the
    /// caps are enforced against.
    ///
    /// Centred on the world origin, which is where this game's level already is.
    pub fn new(
        extent_x: f32,
        extent_z: f32,
        spacing: f32,
        min_y: f32,
        max_y: f32,
    ) -> Result<Self, MapFault> {
        if !spacing.is_finite() || !(MIN_SPACING..=MAX_SPACING).contains(&spacing) {
            return Err(MapFault::Spacing);
        }
        if !extent_x.is_finite() || !extent_z.is_finite() || extent_x <= 0.0 || extent_z <= 0.0 {
            return Err(MapFault::Extent);
        }
        // Rounded, not truncated: an author asking for 500 m at 3 m spacing wants the nearest grid
        // to that, not one that quietly comes up two metres short.
        let along = |extent: f32| (extent / spacing).round() as i64 + 1;
        let nx = along(extent_x).clamp(2, i64::from(u32::MAX)) as u32;
        let nz = along(extent_z).clamp(2, i64::from(u32::MAX)) as u32;
        // Centred on what the grid *is*, not on what was asked for. The two differ whenever the
        // extent is not a whole number of spacings — 500 m at 3 m spacing comes out as 501 — and
        // centring on the request would leave the map half a metre off the origin for no reason a
        // reader could see.
        let grid = Self {
            nx,
            nz,
            spacing,
            origin_x: -((nx - 1) as f32) * spacing / 2.0,
            origin_z: -((nz - 1) as f32) * spacing / 2.0,
            min_y,
            max_y,
        };
        grid.check()?;
        Ok(grid)
    }

    /// Whether this grid is one the server will allocate for and send.
    ///
    /// Separate from [`new`](Self::new) because a grid also arrives already built, out of a
    /// manifest somebody may have edited by hand, and it has to face the same questions then.
    pub fn check(&self) -> Result<(), MapFault> {
        if !self.spacing.is_finite() || !(MIN_SPACING..=MAX_SPACING).contains(&self.spacing) {
            return Err(MapFault::Spacing);
        }
        if !self.origin_x.is_finite() || !self.origin_z.is_finite() {
            return Err(MapFault::Extent);
        }
        if !self.min_y.is_finite() || !self.max_y.is_finite() || self.max_y <= self.min_y {
            return Err(MapFault::Range);
        }
        if self.nx < 2 || self.nz < 2 {
            return Err(MapFault::Extent);
        }
        if self.nx > MAX_SAMPLES_PER_AXIS || self.nz > MAX_SAMPLES_PER_AXIS {
            return Err(MapFault::TooLongAnAxis);
        }
        // As `u64`, because the whole point of this check is a product that does not fit.
        if u64::from(self.nx) * u64::from(self.nz) > u64::from(MAX_SAMPLES) {
            return Err(MapFault::TooManySamples);
        }
        Ok(())
    }

    /// How many samples this grid holds. Only ever called on a checked grid, so it cannot overflow.
    pub fn samples(&self) -> usize {
        self.nx as usize * self.nz as usize
    }

    /// How many metres across the map is, per axis. The inverse of what an author typed.
    pub fn extent_x(&self) -> f32 {
        (self.nx - 1) as f32 * self.spacing
    }

    pub fn extent_z(&self) -> f32 {
        (self.nz - 1) as f32 * self.spacing
    }

    /// The vertical range there is to spend.
    pub fn span(&self) -> f32 {
        self.max_y - self.min_y
    }

    /// Where in the world one sample sits, on the ground plane.
    pub fn world_of(&self, ix: u32, iz: u32) -> Vec2 {
        Vec2::new(
            self.origin_x + ix as f32 * self.spacing,
            self.origin_z + iz as f32 * self.spacing,
        )
    }

    /// The corners of the map on the ground plane: the first and last sample of each axis.
    ///
    /// The footprint a marker has to stand inside, and the rim past which `height_over` clamps
    /// rather than answers. Named once so that "on the map" means one thing.
    pub fn bounds(&self) -> (Vec2, Vec2) {
        (self.world_of(0, 0), self.world_of(self.nx - 1, self.nz - 1))
    }

    /// Where in the heights one sample lives. Row-major, x fastest — the order the blob is in.
    pub fn index(&self, ix: u32, iz: u32) -> usize {
        iz as usize * self.nx as usize + ix as usize
    }

    /// A world height as a sample value.
    ///
    /// Saturating rather than wrapping at both ends: a brush that pushes past the range should
    /// stop at it, and the alternative is a mountain that comes out as a pit.
    pub fn quantise(&self, y: f32) -> u16 {
        let unit = ((y - self.min_y) / self.span()).clamp(0.0, 1.0);
        (unit * f32::from(u16::MAX)).round() as u16
    }

    /// A sample value as a world height. The exact inverse of [`quantise`](Self::quantise) on the
    /// lattice, which is what makes a stroke that reads and writes the field idempotent.
    pub fn height(&self, sample: u16) -> f32 {
        self.min_y + f32::from(sample) / f32::from(u16::MAX) * self.span()
    }

    /// The sample a fresh map is filled with.
    ///
    /// The midpoint of the range, and not the floor of it: sculpting goes both ways, and a field
    /// that starts at its bottom can only be raised. The first attempt to carve a riverbed would
    /// clamp, and an author would have to lift the whole map before they could dig anything.
    ///
    /// The exact midpoint is 32767.5, which is not a sample, so this is the one above it — half a
    /// step high, or 1 mm on a 128 m range.
    pub fn midpoint(&self) -> u16 {
        self.quantise((self.min_y + self.max_y) / 2.0)
    }
}

/// A map's heights, and the sea over them.
///
/// The heights are the bulk of the file and the whole of the wire baseline; everything else about a
/// map is small and lives in [`Manifest`].
#[derive(Clone, Debug, PartialEq)]
pub struct Terrain {
    pub grid: Grid,
    /// World y of the sea surface, or `None` for a dry map.
    ///
    /// Water is *everywhere* at this height and the field only says where the bottom is close
    /// enough to matter — which is what makes the shore rule, the underwater tint and the splash
    /// test one comparison against a plane, with no "is there terrain here" case in front of them.
    pub water_y: Option<f32>,
    /// `nx * nz` samples, row-major, x fastest.
    pub heights: Vec<u16>,
    /// What the ground looks like, as rules rather than as pixels. See [`Layer`].
    pub layers: Vec<Layer>,
}

impl Terrain {
    /// A new map: flat at half height, everywhere.
    pub fn new(
        extent_x: f32,
        extent_z: f32,
        spacing: f32,
        min_y: f32,
        max_y: f32,
    ) -> Result<Self, MapFault> {
        let grid = Grid::new(extent_x, extent_z, spacing, min_y, max_y)?;
        Ok(Self {
            heights: vec![grid.midpoint(); grid.samples()],
            grid,
            water_y: None,
            layers: default_layers(),
        })
    }

    /// The height at one sample, in metres.
    pub fn height_at(&self, ix: u32, iz: u32) -> f32 {
        self.grid.height(self.heights[self.grid.index(ix, iz)])
    }

    /// The ground under a point, in metres, between the samples.
    ///
    /// Bilinear across the cell the point falls in, and clamped at the rim rather than wrapping or
    /// failing: past the edge of the map the nearest edge sample is the honest answer, and every
    /// caller so far is asking where to put something rather than whether the map reaches.
    ///
    /// This is how anything is placed *on* the ground without going through a collider — which is
    /// what a map switch needs, since the collider for the new map does not exist yet when the
    /// spawn points have to be found.
    pub fn height_over(&self, x: f32, z: f32) -> f32 {
        let grid = self.grid;
        let along = |world: f32, origin: f32, n: u32| {
            let cell = ((world - origin) / grid.spacing).clamp(0.0, (n - 1) as f32);
            let low = (cell.floor() as u32).min(n - 2);
            (low, (cell - low as f32).clamp(0.0, 1.0))
        };
        let (ix, tx) = along(x, grid.origin_x, grid.nx);
        let (iz, tz) = along(z, grid.origin_z, grid.nz);
        let (h00, h10) = (self.height_at(ix, iz), self.height_at(ix + 1, iz));
        let (h01, h11) = (self.height_at(ix, iz + 1), self.height_at(ix + 1, iz + 1));
        let south = h00 + (h10 - h00) * tx;
        let north = h01 + (h11 - h01) * tx;
        south + (north - south) * tz
    }

    /// How far the ground at one sample sits **below** the ground around it, in metres.
    ///
    /// Positive in a hollow, zero on a plane, negative on a ridge — the average of four samples a
    /// span away, minus this one. It is the one input to the look that is not a property of the
    /// point by itself, and it is what puts dirt where dirt goes.
    ///
    /// The span is [`DIP_SPAN_METRES`] and not the neighbouring sample, which matters more than it
    /// looks: comparing a sample with the ones a metre away measures the *noise* of the height
    /// field, not the shape of the land, and would scatter dirt over every dimple on an otherwise
    /// clean hillside. A valley is a wide thing and has to be asked about at its own width.
    ///
    /// At the rim the samples clamp, exactly as the normals do, so the edge of the map reads as
    /// flat ground rather than as a trench.
    pub fn dip_at(&self, ix: u32, iz: u32) -> f32 {
        let grid = self.grid;
        let span = (DIP_SPAN_METRES / grid.spacing).round().max(1.0) as u32;
        let at = |x: u32, z: u32| self.height_at(x.min(grid.nx - 1), z.min(grid.nz - 1));
        let around = (at(ix.saturating_sub(span), iz)
            + at(ix + span, iz)
            + at(ix, iz.saturating_sub(span))
            + at(ix, iz + span))
            / 4.0;
        around - self.height_at(ix, iz)
    }

    /// The heights as they go on disk and on the wire: little-endian `u16`, row-major, x fastest.
    ///
    /// Little-endian stated rather than assumed. Every machine this runs on is little-endian, so
    /// `to_le_bytes` costs nothing today; writing it down is what stops the file format from
    /// quietly being "whatever this machine does".
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.heights.len() * 2);
        for sample in &self.heights {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    /// The other half, and the place the two files are made to agree.
    ///
    /// The grid is checked first and the blob's length second, because a grid that fails its caps
    /// gives a length nobody should be allocating against.
    pub fn decode(grid: Grid, water_y: Option<f32>, blob: &[u8]) -> Result<Self, MapFault> {
        grid.check()?;
        let expected = grid.samples() * 2;
        if blob.len() != expected {
            return Err(MapFault::BlobSize { expected, found: blob.len() });
        }
        let heights = blob
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Ok(Self { grid, water_y, heights, layers: default_layers() })
    }

}

impl Grid {
    /// How many cells of height field one tile covers, in both the collider and the picture.
    ///
    /// The same number for both, and that is the whole point of it being here rather than in the
    /// client: a stroke dirties tiles, and a dirty tile rebuilds its mesh *and* its collider. Two
    /// tilings would mean a stroke rebuilding a different set of each, and the two drifting apart
    /// in what they cover.
    ///
    /// 64 also gives frustum culling something to work with. It does not give it much on a map
    /// this size — from ground level most of a 512 m map is visible — but it is the unit editing
    /// wants, which is the stronger reason.
    pub const TILE_CELLS: u32 = 64;

    /// How many tiles across and down. Always at least one, and the last of each row is short
    /// whenever the map is not a whole number of tiles.
    pub fn tiles(&self) -> (u32, u32) {
        let along = |n: u32| (n - 1).div_ceil(Self::TILE_CELLS).max(1);
        (along(self.nx), along(self.nz))
    }

    /// The samples one tile owns, inclusive at both ends.
    ///
    /// Neighbouring tiles **share** their boundary samples, and that is not an off-by-one: a tile
    /// of `n` cells needs `n + 1` grid points, and a tiling that gave each tile its own edge would
    /// leave a one-cell strip of nothing between every pair of them.
    pub fn tile_samples(&self, tx: u32, tz: u32) -> (u32, u32, u32, u32) {
        let ix0 = tx * Self::TILE_CELLS;
        let iz0 = tz * Self::TILE_CELLS;
        (
            ix0,
            (ix0 + Self::TILE_CELLS).min(self.nx - 1),
            iz0,
            (iz0 + Self::TILE_CELLS).min(self.nz - 1),
        )
    }

    /// Which tiles a patch of samples falls in, inclusive.
    pub fn tiles_over(&self, ix0: u32, ix1: u32, iz0: u32, iz1: u32) -> (u32, u32, u32, u32) {
        let (wide, deep) = self.tiles();
        // A sample on a boundary belongs to both tiles that share it, so the low end is rounded
        // down by one cell before dividing. Rebuilding one tile too many is invisible; rebuilding
        // one too few leaves a seam standing where the ground has moved.
        let low = |i: u32| i.saturating_sub(1) / Self::TILE_CELLS;
        let high = |i: u32, count: u32| (i / Self::TILE_CELLS).min(count - 1);
        (low(ix0), high(ix1, wide), low(iz0), high(iz1, deep))
    }
}

impl Terrain {
    /// One tile of the ground as a collider, and where to put it.
    ///
    /// The same construction as the whole-map collider below, over a window of the samples — and it
    /// has the same two traps, which is why there is one place that builds a height field and both
    /// callers go through it.
    pub fn tile_collider(&self, tx: u32, tz: u32) -> (Collider, Vec3) {
        let grid = self.grid;
        let (ix0, ix1, iz0, iz1) = grid.tile_samples(tx, tz);
        self.height_field(ix0, ix1, iz0, iz1)
    }

    /// The shape the ground is, and where to put it.
    ///
    /// Avian takes a nested `Vec<Vec<Scalar>>` and derives the grid dimensions from the structure,
    /// which eats a whole class of mistake the equivalent Rapier call leaves open. Two traps
    /// survive it and both fail *silently*, so both are settled by
    /// [`the_ramp_runs_the_way_it_was_built`](tests::the_ramp_runs_the_way_it_was_built) against
    /// the installed crate rather than by reading anybody's documentation:
    ///
    /// - **Which index is which axis.** Avian's own doc comment says the number of rows is the
    ///   subdivisions along X. Underneath, parry's `Array2` is *column-major* —
    ///   `flat_index(i, j) = i + j * nrows` — while Avian flattens the nesting row-major, and
    ///   parry's accessors read `j` as x and `i` as z. Getting it wrong transposes the terrain
    ///   about its diagonal with no error of any kind: a ramp authored along +X comes out running
    ///   along +Z.
    /// - **Centring.** parry's height field is centred on its own origin, so the body belongs at
    ///   the field's *centre* — not at `origin`, which points at the min corner.
    ///
    /// The heights are handed over in metres and the vertical scale is 1, rather than passing a
    /// span and letting parry multiply. One less place for a factor to be applied twice.
    pub fn collider(&self) -> (Collider, Vec3) {
        let grid = self.grid;
        self.height_field(0, grid.nx - 1, 0, grid.nz - 1)
    }

    /// A height field over one window of the samples, inclusive at both ends, and where its body
    /// belongs.
    ///
    /// The one place a height field is built, which is what keeps both traps above settled in one
    /// place rather than in every caller that wants a piece of the ground.
    fn height_field(&self, ix0: u32, ix1: u32, iz0: u32, iz1: u32) -> (Collider, Vec3) {
        let grid = self.grid;
        let (nx, nz) = ((ix1 - ix0 + 1) as usize, (iz1 - iz0 + 1) as usize);
        // parry's `Array2` is column-major — `flat_index(i, j) = i + j * nrows` — and its accessors
        // read `i` as z and `j` as x. So the data has to run z-fastest, with `nrows` counting the
        // samples along z.
        let mut data = vec![0.0f32; nx * nz];
        for ix in 0..nx {
            for iz in 0..nz {
                data[iz + ix * nz] = self.height_at(ix0 + ix as u32, iz0 + iz as u32);
            }
        }
        let heights = Array2::new(nz, nx, data);
        let (low, high) = (grid.world_of(ix0, iz0), grid.world_of(ix1, iz1));
        let centre = Vec3::new((low.x + high.x) / 2.0, 0.0, (low.y + high.y) / 2.0);
        let scale = Vec3::new(high.x - low.x, 1.0, high.y - low.y);
        (Collider::from(SharedShape::heightfield(heights, scale)), centre)
    }

    /// What goes in the `.json` beside the blob.
    pub fn manifest(&self) -> Manifest {
        Manifest {
            version: VERSION,
            grid: self.grid,
            water_y: self.water_y,
            layers: self.layers.clone(),
            markers: Vec::new(),
        }
    }
}

/// The map this world is played on.
///
/// A resource rather than a component: there is one authoritative height field and no timeline on
/// which a second version of it means anything, which is also why it does not travel through
/// component replication — see [`TerrainBaseline`] and terrain.md §3.
///
/// The server has one from the moment it starts. **A client does not**, and its absence is the
/// whole of "the map has not arrived yet": every system that needs ground is gated on this
/// existing, which is cheaper and harder to get wrong than a flag saying the same thing. A client
/// must never load the map itself — once terrain is editable the file on disk is the *last saved*
/// state, so a client that read it would walk on different ground from everyone else for as long
/// as anybody held an unsaved edit.
#[derive(Resource, Clone, Debug)]
pub struct Ground(pub Terrain);

/// The whole map, on its way to a client that has just joined.
///
/// Half a megabyte on the default map, which is why it has a reliable channel of its own rather
/// than a place in the replication stream: a baseline must never delay a position update, and a
/// position update must never delay a baseline into next round.
///
/// The heights travel as [`Terrain::encode`] writes them rather than as `Vec<u16>`, so the wire
/// format and the file format are one definition and cannot drift apart.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct TerrainBaseline {
    pub grid: Grid,
    pub water_y: Option<f32>,
    pub heights: Vec<u8>,
    /// Strokes that have been accepted but whose tick has not come.
    ///
    /// Without these a client that joins in the half-second between a stroke being broadcast and
    /// being applied would get ground that has not had it yet and would never hear about it —
    /// standing, from then on, on a map nobody else has. They travel with the baseline because they
    /// belong to it: the pair is "the ground, and what is still on its way to it".
    #[serde(default)]
    pub pending: Vec<crate::sculpt::TerrainEdit>,
    /// The map's own look. Empty means "whatever this build calls default", which is what an older
    /// server sends and what a map written before there were rules holds.
    #[serde(default)]
    pub layers: Vec<Layer>,
}

impl TerrainBaseline {
    /// What the server sends.
    pub fn of(terrain: &Terrain, pending: &[crate::sculpt::TerrainEdit]) -> Self {
        Self {
            grid: terrain.grid,
            water_y: terrain.water_y,
            heights: terrain.encode(),
            pending: pending.to_vec(),
            layers: terrain.layers.clone(),
        }
    }

    /// What the client makes of it, and the place a hostile or broken baseline is refused.
    ///
    /// Goes through [`Terrain::decode`], so the caps and the blob-length check that guard a map
    /// read off disk guard one read off the wire as well. That matters more here, not less: this
    /// arrives from another machine.
    pub fn adopt(&self) -> Result<Terrain, MapFault> {
        let mut terrain = Terrain::decode(self.grid, self.water_y, &self.heights)?;
        // An empty list is "whatever this build calls default" rather than "a map with no look":
        // there is no way to author a map with nothing on it, and a black world is a worse answer
        // to an old server than the default one.
        if !self.layers.is_empty() {
            terrain.layers = self.layers.clone();
        }
        Ok(terrain)
    }
}

/// The readable half of a map: everything that is not a height.
///
/// JSON through `serde`, and every added field defaulted, because this is the part that changes
/// constantly while the design is young. A binary manifest would make each new field a codec
/// change, a version bump and a migration for maps that already exist — paid on every field,
/// including the ones that turn out to be wrong a week later.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub grid: Grid,
    #[serde(default)]
    pub water_y: Option<f32>,
    /// Up to four, each naming an ordinary material asset. Empty until step four wants them.
    #[serde(default)]
    pub layers: Vec<Layer>,
    /// What has been placed on the map. Empty until step eight wants them.
    #[serde(default)]
    pub markers: Vec<Marker>,
}

impl Manifest {
    /// Whether this is a map this build can open, before anything is allocated for it.
    pub fn check(&self) -> Result<(), MapFault> {
        if self.version > VERSION {
            return Err(MapFault::Version(self.version));
        }
        self.grid.check()
    }
}

/// One surface the ground can wear, and the rule that decides where it appears.
///
/// **There is no splat map.** Which surface shows at a point is a function of that point's slope
/// and its height, evaluated per pixel in the fragment shader — terrain.md's *Surface appearance is
/// derived* argument in full. Nothing is painted, so there is no second grid on disk or on the
/// wire, no paint brush, no per-layer weight bookkeeping, and no "regenerate the texturing" action
/// that destroys somebody's afternoon. What it costs is stated there too: a map can never have a
/// patch of moss the ground's own shape does not explain.
///
/// The rule travels with the map rather than living in the client, so a map can look like itself.
/// The *evaluation* is duplicated between here and WGSL — terrain.md §10 — and the numbers are not:
/// they are uploaded from these fields into a uniform, so the two copies cannot disagree about the
/// bands, only about the arithmetic between them, which is what
/// [`the_shader_and_the_cpu_agree`](tests::the_shader_and_the_cpu_agree) is arranged to pin.
///
/// `texture` and `tile_scale` are the plan's own fields and are carried unused: there are no ground
/// textures in the repository yet, so a layer is a colour and a roughness for now. Nothing about
/// the rule changes when they arrive — a texture is sampled *instead of* the flat colour, at the
/// weight this already computes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// What to call it, and what the texture will be named when there is one.
    pub texture: String,
    /// Metres per texture repeat. Unused until there is a texture; kept so a map written now does
    /// not need a migration when there is.
    #[serde(default = "one")]
    pub tile_scale: f32,
    /// Linear RGB.
    pub colour: [f32; 3],
    pub roughness: f32,
    /// Which slopes, in **degrees from flat**: 0 is level ground, 90 is a wall.
    ///
    /// Degrees in the file and in the code both, and converted to a cosine nowhere — the shader
    /// takes the angle too. Readability is the whole reason the manifest is JSON, and `35` is a
    /// slope where `0.819` is a number somebody has to work out.
    pub slope: Band,
    /// Which heights, in metres of world y.
    pub height: Band,
    /// How deep a hollow, in **metres below the ground around it** — see [`Terrain::dip_at`].
    ///
    /// The third axis, and the one that is not a property of the point alone. Slope and height ask
    /// what a place *is*; this asks what is above it, which is what decides where anything loose
    /// ends up. Dirt collects in hollows and gullies for the same reason water does, and no
    /// combination of the other two says "hollow": a valley floor is flat and can sit at any
    /// height at all.
    ///
    /// Defaulted, so a manifest written before this existed still reads.
    #[serde(default = "anything")]
    pub dip: Band,
}

fn one() -> f32 {
    1.0
}

fn anything() -> Band {
    Band::ANY
}

/// A range with a soft edge: full weight inside, fading to nothing across `blend` at each end.
///
/// The fade is **centred on the edge** — half of it inside the band and half outside — which is
/// what makes two layers that share an edge sum to exactly one across the whole crossing, so the
/// ground never darkens or washes out in the seam between them. A fade hung entirely outside the
/// band would leave both layers at full weight at the edge itself.
/// [`two_bands_sharing_an_edge_sum_to_one`](tests::two_bands_sharing_an_edge_sum_to_one) is that
/// property.
///
/// The fade itself is a **smoothstep**, not a ramp. A ramp sums to one just as exactly, but it
/// arrives at each end of the crossing with a corner in it, and a corner in a weight is a visible
/// line on the ground — the eye finds the second derivative of a shading gradient far more readily
/// than the first. Smoothstep is symmetric about its middle, `s(1 - t) = 1 - s(t)`, so the sum
/// survives it untouched.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub from: f32,
    pub to: f32,
    /// How wide the fade is at each end. Zero is a hard edge.
    pub blend: f32,
}

impl Band {
    /// Everything, with no edges to fade.
    pub const ANY: Band = Band { from: -1.0e9, to: 1.0e9, blend: 0.0 };

    /// How much of this band a value is inside, from 0 to 1.
    pub fn weight(self, x: f32) -> f32 {
        let blend = self.blend.max(1.0e-4);
        let rising = smoothstep(((x - self.from) / blend + 0.5).clamp(0.0, 1.0));
        let falling = smoothstep(((self.to - x) / blend + 0.5).clamp(0.0, 1.0));
        rising.min(falling)
    }
}

impl Layer {
    /// How much of this layer shows on a surface at this angle, this height and this depth of
    /// hollow.
    ///
    /// The CPU half of the derivation. Nothing in the game reads it yet — footstep sounds and
    /// impact decals are what terrain.md §10 says will — but it is what the shader is tested
    /// against, and a rule with no way to ask it from Rust is a rule nobody can test.
    pub fn weight(&self, slope_degrees: f32, y: f32, dip: f32) -> f32 {
        self.slope.weight(slope_degrees) * self.height.weight(y) * self.dip.weight(dip)
    }
}

/// How far out [`Terrain::dip_at`] looks for "the ground around it", in metres.
///
/// `webgame`'s `valleySpanM`, and the same twelve metres: wide enough that a hollow has to be a
/// feature of the landscape rather than a bump in the sampling, narrow enough that a gully still
/// counts as one.
pub const DIP_SPAN_METRES: f32 = 12.0;

/// How steep a surface is, in degrees from flat, given the y of its unit normal.
pub fn slope_degrees(normal_y: f32) -> f32 {
    normal_y.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Which layer shows most strongly at a point, if any does.
///
/// The question the CPU side of terrain.md §10 actually wants answered — *what am I standing on* —
/// rather than the weights, which are the shader's business.
pub fn surface_of(layers: &[Layer], normal_y: f32, y: f32, dip: f32) -> Option<usize> {
    let slope = slope_degrees(normal_y);
    layers
        .iter()
        .enumerate()
        .map(|(index, layer)| (index, layer.weight(slope, y, dip)))
        .filter(|(_, weight)| *weight > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// How many layers a map may have, which is what the shader's uniform is sized for.
pub const MAX_LAYERS: usize = 4;

/// The look a map gets when it does not describe one.
///
/// The three `webgame` puts on its own hills map, by name and at its tile scale, because the two
/// games are meant to feel like the same place: **Grass001** on open ground, **Ground048** in the
/// hollows it drains into, **Rock020** where it is too steep to hold anything. All three are
/// ambientCG packs under CC0 — see `assets/CREDITS.md`.
///
/// **Two questions, asked in that order, and each one splits the set in half.** Is it steep? Then
/// rock, and neither of the other two. Otherwise: does it sit below what surrounds it? Then dirt,
/// else grass. Every pair that shares an edge shares its blend as well, so the weights sum to one
/// through each crossing rather than merely near it.
///
/// The steep edge is 35° with a 16° crossing, which is `webgame`'s `steepDeg` and twice its
/// `steepBlendDeg` — the same numbers because the same crossing is meant. Grass to 27°, rock from
/// 43°, and the transition between. **This map is not gentle**: measured over the built-in
/// terrain, 30% of it lies between 15° and 30°. A middle *slope* band anywhere in that range —
/// which is what stood here before — paints a fifth of the whole map brown in scattered patches
/// on ground that is plainly a grassy hillside. The band was doing nothing but marking the
/// gradient of the hill.
///
/// Height carries no rule at all. It is what a shore line and a snow line are made of, and this
/// map has neither: there is no water yet, so a sand band round y = 0 would put a beach across the
/// flat ground everybody spawns on, and there is no snow texture for a summit. A rule that cannot
/// be seen is a rule that cannot be checked, so it waits for the thing that makes it visible.
///
/// A layer's `colour` is what its texture *averages* to in **linear** light, and it does three
/// jobs: it is what the ground is painted with before the texture has loaded, it is what a
/// headless build sees, and it is the mean the shader's stochastic blend restores the contrast
/// around. Measured off each `_Color.png` — every one of these was previously an sRGB-looking
/// value written into a linear field, four to seven times too bright, and grass had lost most of
/// its green with it.
pub fn default_layers() -> Vec<Layer> {
    /// Where grass gives way to rock, and how wide the crossing is.
    const STEEP: Band = Band { from: 35.0, to: 1.0e9, blend: 16.0 };
    const NOT_STEEP: Band = Band { from: -1.0e9, to: 35.0, blend: 16.0 };
    /// How deep a hollow has to be before it is a hollow, over the span `dip_at` measures.
    const HOLLOW: Band = Band { from: 1.5, to: 1.0e9, blend: 1.5 };
    const NOT_HOLLOW: Band = Band { from: -1.0e9, to: 1.5, blend: 1.5 };

    vec![
        Layer {
            texture: "Grass001_1K-PNG".into(),
            tile_scale: 4.0,
            colour: [0.060, 0.109, 0.023],
            roughness: 0.95,
            slope: NOT_STEEP,
            height: Band::ANY,
            dip: NOT_HOLLOW,
        },
        Layer {
            texture: "Ground048_1K-PNG".into(),
            tile_scale: 4.0,
            colour: [0.105, 0.056, 0.038],
            roughness: 0.92,
            slope: NOT_STEEP,
            height: Band::ANY,
            dip: HOLLOW,
        },
        Layer {
            texture: "Rock020_1K-PNG".into(),
            tile_scale: 4.0,
            colour: [0.084, 0.082, 0.069],
            roughness: 0.8,
            slope: STEEP,
            height: Band::ANY,
            dip: Band::ANY,
        },
    ]
}


/// How high above the ground a marker may sit, either way.
///
/// A relative height needs a cap in *both* directions, which an absolute one would not: a negative
/// offset is legitimate — a crate half sunk into a slope, a spawn in a dip — and a large one is a
/// spawn under the map. Twenty metres is taller than anything this game stacks and well inside the
/// height range of the built-in map.
pub const MAX_MARKER_Y: f32 = 20.0;

/// How many markers one map may hold.
///
/// Not a memory bound — a marker is around thirty bytes and this is a kilobyte and a half. It is
/// the bound on the marker list staying the readable thing terrain.md §7 says it is, and the point
/// past which somebody has built a forest by hand instead of seeding one.
pub const MAX_MARKERS: usize = 512;

/// The name of the marker kind a player starts on.
///
/// Spelled once, because two places in the server ask "is there still somewhere to spawn" and a
/// map with no answer is unplayable.
pub const PLAYER_SPAWN: &str = "player";

/// Something placed on the map: where it goes, not what it is.
///
/// A marker is map content — a kind, a place and a facing. It has no collider and no hitbox, it
/// never enters prediction, and the *entity* it produces at round start is ordinary gameplay state
/// that knows nothing about it. That separation is what makes a round reset a re-read rather than a
/// special case.
///
/// The height is **relative to the ground**, not a world y, and that is a correctness rule rather
/// than a saving: terrain is editable, so an absolute height is wrong the moment somebody sculpts
/// underneath it — a spawn buried in a new hill, a vehicle dropped four metres onto a valley floor
/// that used to be a ridge. An offset moves with the ground, so every marker survives every stroke
/// with no fix-up pass and no way to forget one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "MarkerFile", into = "MarkerFile")]
pub struct Marker {
    /// Which placeable this is, as a palette id rather than an enum of the three that exist today.
    pub kind: String,
    pub x: f32,
    pub z: f32,
    /// Metres above the ground beneath, not world y.
    pub y: f32,
    /// A full rotation, because a yaw cannot say "lying the way that hillside does".
    ///
    /// Not Euler angles as the representation, and not for gimbal lock: **a quaternion has no
    /// convention to disagree about.** Three angles need an axis order and a handedness, both sides
    /// have to pick the same ones, and the failure when they do not is a marker that is subtly
    /// turned rather than an error anybody sees.
    pub rotation: Quat,
}

impl Marker {
    /// Whether this is a marker a map can hold, given the grid it stands on.
    ///
    /// The same check for one read off disk and one off the wire, which is the point of it being
    /// here: the file is hand-editable and the wire is another machine, and neither is trusted.
    pub fn check(&self, grid: Grid) -> Result<(), MapFault> {
        if self.kind.is_empty() || self.kind.len() > 64 {
            return Err(MapFault::Marker);
        }
        if !self.x.is_finite() || !self.z.is_finite() || !self.y.is_finite() {
            return Err(MapFault::Marker);
        }
        let (low, high) = grid.bounds();
        if self.x < low.x || self.x > high.x || self.z < low.y || self.z > high.y {
            return Err(MapFault::Marker);
        }
        if self.y.abs() > MAX_MARKER_Y {
            return Err(MapFault::Marker);
        }
        // An unnormalised quaternion does not error when it is used, it scales and skews whatever
        // it is applied to. So the length is checked rather than assumed — and `normalize` is what
        // fixes a merely sloppy one, in `MarkerFile`'s conversion, before it ever gets here.
        if !self.rotation.is_finite() || (self.rotation.length() - 1.0).abs() > 1.0e-3 {
            return Err(MapFault::Marker);
        }
        Ok(())
    }

    /// Where the entity this marker describes actually appears, on the ground as it is now.
    pub fn where_it_stands(&self, terrain: &Terrain) -> Vec3 {
        Vec3::new(self.x, terrain.height_over(self.x, self.z) + self.y, self.z)
    }
}

/// How near a rotation has to be to a pure yaw before the file writes it as one.
///
/// A tolerance rather than an equality, because a rotation that has been through a `Quat` and back
/// is not bit-identical to the one that went in. Loose enough to catch that, tight enough that
/// anything an author actually tilted is written in full: at this bound the axis is under a
/// thousandth of a degree off vertical.
const PURE_YAW: f32 = 1.0e-6;

/// A marker the way the manifest writes it, which is not quite the way the game holds it.
///
/// Two differences, both for the sake of the file being read by people. The rotation may be given
/// as a `yaw` in **degrees** instead of four numbers, because `[0, 0.383, 0, 0.924]` is not
/// something anybody can check by looking. And `y` is left out when it is zero, which is the
/// ordinary case.
///
/// The shorthand is safe because it has an exact expansion *and* an exact contraction: a pure yaw
/// can be recognised on the way out and written back as one. That is what stops it being
/// write-only — an "accept either" reader paired with an "always write the general form" writer
/// turns every hand-written `yaw: 45` into a quaternion on the first save, and the readability
/// lasts exactly as long as nobody edits the map.
#[derive(Clone, Serialize, Deserialize)]
struct MarkerFile {
    kind: String,
    x: f32,
    z: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    y: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rotation: Option<[f32; 4]>,
    /// Degrees, and degrees only here — radians everywhere in code, converted at this boundary and
    /// nowhere else. `"yaw": 45` is readable where `"yaw": 0.785` is not, and readability is the
    /// entire reason the shorthand exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    yaw: Option<f32>,
}

fn is_zero(y: &f32) -> bool {
    *y == 0.0
}

impl TryFrom<MarkerFile> for Marker {
    type Error = MapFault;

    fn try_from(file: MarkerFile) -> Result<Self, MapFault> {
        let rotation = match (file.rotation, file.yaw) {
            (Some(_), Some(_)) => return Err(MapFault::TwoRotations),
            (Some([x, y, z, w]), None) => {
                // Normalised rather than trusted: a hand-written quaternion is rarely unit length,
                // and one that is not silently stretches the thing it turns.
                Quat::from_xyzw(x, y, z, w).normalize()
            }
            // The convention `level.rs` already states and uses: a yaw about +Y, applied to the −Z
            // that is forward everywhere here.
            (None, Some(degrees)) => Quat::from_rotation_y(degrees.to_radians()),
            (None, None) => Quat::IDENTITY,
        };
        if !rotation.is_finite() {
            return Err(MapFault::Marker);
        }
        Ok(Marker { kind: file.kind, x: file.x, z: file.z, y: file.y, rotation })
    }
}

impl From<Marker> for MarkerFile {
    fn from(marker: Marker) -> Self {
        let q = marker.rotation;
        // Canonical, not remembered: whatever the file said before, a rotation that *is* a yaw is
        // written as one. `to_euler` is exact for a rotation about Y alone, and the branch is what
        // decides it — a tilted marker keeps all four numbers.
        let yaw = (q.x.abs() < PURE_YAW && q.z.abs() < PURE_YAW)
            .then(|| q.to_euler(EulerRot::YXZ).0.to_degrees());
        Self {
            kind: marker.kind,
            x: marker.x,
            z: marker.z,
            y: marker.y,
            rotation: yaw.is_none().then(|| q.to_array()),
            yaw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::WALKABLE_NORMAL_Y;
    use crate::physics::test_support::{ask, bare_app};

    /// A terrain that is flat along z and a ramp along x, so a transpose cannot hide.
    ///
    /// Deliberately not square either: parry's `Array2` is column-major and Avian flattens the
    /// nesting row-major, and those two only agree when the sample counts match. A square probe
    /// would pass whatever the answer is, which is the same reason the ramp is asymmetric.
    fn asymmetric_ramp() -> Terrain {
        let mut terrain = Terrain::new(32.0, 16.0, 8.0, -64.0, 64.0).expect("a probe");
        assert_eq!((terrain.grid.nx, terrain.grid.nz), (5, 3), "the probe is not square");
        for ix in 0..terrain.grid.nx {
            for iz in 0..terrain.grid.nz {
                let index = terrain.grid.index(ix, iz);
                terrain.heights[index] = terrain.grid.quantise(ix as f32 * 2.0);
            }
        }
        terrain
    }

    /// The probe world: this terrain as the only thing in it.
    fn probe(terrain: &Terrain) -> App {
        let mut app = bare_app();
        let (collider, centre) = terrain.collider();
        app.world_mut().spawn(crate::physics::level_geometry(collider, centre));
        app.update();
        app
    }

    /// Straight down from well overhead, and the world y it landed on.
    fn ground_at(app: &mut App, x: f32, z: f32) -> Option<f32> {
        ask(app, move |level| level.raycast(Vec3::new(x, 500.0, z), Vec3::NEG_Y, 1000.0))
            .map(|distance| 500.0 - distance)
    }

    /// Prints the authored field beside the collider that came out of it.
    ///
    /// Ignored, because it is a diagnostic and not a check. When an axis or a stride is wrong the
    /// assertion above says *that* it is wrong; this says *how*, which is the difference between
    /// an afternoon and a minute. A transpose shows as the ramp running down the other axis; a
    /// stride mismatch shows as rows that are each shifted a little further than the last, which is
    /// what Avian's own `Collider::heightfield` produced here.
    ///
    /// Run it with `cargo test -p noob_tube_shared -- --ignored --nocapture`.
    #[test]
    #[ignore = "a diagnostic, not a check"]
    fn print_the_shape_the_collider_came_out() {
        let terrain = asymmetric_ramp();
        println!("authored, height_at(ix, iz) — should climb with ix only:");
        for iz in 0..terrain.grid.nz {
            let row: Vec<String> = (0..terrain.grid.nx)
                .map(|ix| format!("{:6.2}", terrain.height_at(ix, iz)))
                .collect();
            println!("  iz {iz}: {}", row.join(" "));
        }
        let mut app = probe(&terrain);
        println!("collider, sampled on the same lattice:");
        for iz in 0..terrain.grid.nz {
            let z = terrain.grid.world_of(0, iz).y * 0.98;
            let row: Vec<String> = (0..terrain.grid.nx)
                .map(|ix| {
                    let x = terrain.grid.world_of(ix, 0).x * 0.98;
                    match ground_at(&mut app, x, z) {
                        Some(y) => format!("{y:6.2}"),
                        None => "  ----".to_string(),
                    }
                })
                .collect();
            println!("  iz {iz}: {}", row.join(" "));
        }
    }

    /// The one test this whole step turns on: does the ground run the way it was authored?
    ///
    /// Both of the traps here fail silently, and a casual test cannot catch either. A transpose
    /// swaps x for z, so a symmetric shape comes out identical; a centring mistake shifts the field
    /// by half its extent, which a field centred on the origin already is. Hence a ramp that is
    /// asymmetric in shape *and* on a grid that is not square.
    #[test]
    fn the_ramp_runs_the_way_it_was_built() {
        let terrain = asymmetric_ramp();
        let mut app = probe(&terrain);

        // Two metres per sample, eight metres apart, climbing along +x from the min corner at -16.
        for x in [-15.0, -8.0, 0.0, 8.0, 15.0] {
            let want = (x + 16.0) * 0.25;
            let found = ground_at(&mut app, x, 0.0).expect("no ground under the probe at all");
            assert!(
                (found - want).abs() < 0.05,
                "at x {x} the ground is at {found:.2} where the ramp says {want:.2} \
                 — the field is transposed or off-centre",
            );
        }

        // And flat the other way, which is what says it is not merely sloped somewhere.
        for z in [-7.0, 0.0, 7.0] {
            let found = ground_at(&mut app, 0.0, z).expect("no ground");
            assert!((found - 4.0).abs() < 0.05, "at z {z} the ground is at {found:.2}, not flat");
        }
    }

    /// The field covers what the grid says it covers, and stops there.
    #[test]
    fn the_field_reaches_its_own_corners_and_no_further() {
        let terrain = asymmetric_ramp();
        let mut app = probe(&terrain);
        assert!(ground_at(&mut app, -15.9, -7.9).is_some(), "a corner is missing");
        assert!(ground_at(&mut app, 15.9, 7.9).is_some(), "the far corner is missing");
        assert!(ground_at(&mut app, 17.0, 0.0).is_none(), "there is ground past the edge");
        assert!(ground_at(&mut app, 0.0, 9.0).is_none(), "there is ground past the edge");
    }

    /// The map the level starts on.
    #[test]
    fn the_default_map_is_the_canonical_grid() {
        let terrain = default_terrain();
        assert_eq!((terrain.grid.nx, terrain.grid.nz), (513, 513), "the canonical grid");
        assert_eq!(terrain.grid.extent_x(), 512.0);
    }

    /// Flat where the level already stands, so nothing that was placed on the plane is buried by
    /// or left hanging over the ground.
    #[test]
    fn the_ground_the_level_stands_on_is_left_alone() {
        let terrain = default_terrain();
        let grid = terrain.grid;
        for iz in 0..grid.nz {
            for ix in 0..grid.nx {
                let here = grid.world_of(ix, iz);
                if here.length_squared() > HOME_FLAT * HOME_FLAT {
                    continue;
                }
                let y = terrain.height_at(ix, iz);
                assert!(y.abs() < 0.01, "the ground at {here:?} is at {y:.3}, not on the plane");
            }
        }
    }

    /// And not flat anywhere else, which is the whole point of putting hills in it.
    #[test]
    fn there_are_hills_to_climb_and_ravines_to_fall_into() {
        let terrain = default_terrain();
        let grid = terrain.grid;
        let (mut lowest, mut highest) = (f32::MAX, f32::MIN);
        for iz in 0..grid.nz {
            for ix in 0..grid.nx {
                let y = terrain.height_at(ix, iz);
                lowest = lowest.min(y);
                highest = highest.max(y);
            }
        }
        assert!(highest > 15.0, "the tallest thing on the map is {highest:.1} m");
        assert!(lowest < -10.0, "the deepest thing on the map is {lowest:.1} m");
        // And inside the range it was quantised against, or the shaping is being clipped.
        assert!(highest < grid.max_y && lowest > grid.min_y, "the shaping ran out of range");
    }

    /// Nothing outside the tests may reach for a square root or a transcendental.
    ///
    /// Client and server both generate the default map until step five sends it instead, so they
    /// have to agree on it exactly — and `sqrt`, `powf`, `sin` and `hypot` go through `libm`, which
    /// is not required to be correctly rounded and may differ in the last ulp between platforms.
    /// `mul_add` is out for a different reason: it is a *different* result from `a * b + c`,
    /// exactly and deliberately, and whether it lowers to one instruction or two is a property of
    /// the target.
    ///
    /// Everything before `#[cfg(test)]` rather than the generator alone. The first version of this
    /// scanned from `fn default_terrain` to the next occurrence of the same string, which happened
    /// to end where the function does — a boundary that held by accident and would have moved
    /// silently the day somebody wrote that name a third time.
    /// What the server sends is what the client gets, heights and lattice alike.
    ///
    /// The whole of step five rests on this: a client is forbidden to read the map for itself, so
    /// the only copy it will ever have is the one that came out of here.
    #[test]
    fn a_baseline_is_the_map_it_came_from() {
        let sent = default_terrain();
        let arrived = TerrainBaseline::of(&sent, &[]).adopt().expect("a map this build reads");
        assert_eq!(arrived, sent, "the map changed on the way over");
    }

    /// And a baseline that does not add up is refused rather than trusted.
    ///
    /// It arrives from another machine, which is the reason this matters more here than it does
    /// for a file: a grid claiming four million samples is an allocation a client should not make
    /// because somebody asked it to.
    #[test]
    fn a_baseline_that_does_not_add_up_is_refused() {
        let mut short = TerrainBaseline::of(&default_terrain(), &[]);
        short.heights.truncate(short.heights.len() - 2);
        assert!(matches!(short.adopt(), Err(MapFault::BlobSize { .. })), "a short map was adopted");

        let mut vast = TerrainBaseline::of(&default_terrain(), &[]);
        vast.grid.nx = MAX_SAMPLES_PER_AXIS + 1;
        assert!(matches!(vast.adopt(), Err(MapFault::TooLongAnAxis)), "an oversized map was adopted");

        let mut fine = TerrainBaseline::of(&default_terrain(), &[]);
        fine.grid.spacing = MIN_SPACING / 2.0;
        assert!(matches!(fine.adopt(), Err(MapFault::Spacing)), "a map below the spacing cap was adopted");
    }

    /// A name is an allowlist, and the things it must dispose of are paths.
    #[test]
    fn a_name_keeps_only_what_a_map_may_be_called() {
        assert_eq!(sanitise_name("Dust  2").unwrap(), "Dust 2");
        assert_eq!(sanitise_name("  trimmed  ").unwrap(), "trimmed");
        assert_eq!(sanitise_name("under_score-and-dash").unwrap(), "under_score-and-dash");
        // Every one of these is a path, and none of them comes out as one.
        assert_eq!(sanitise_name("../../etc/passwd").unwrap(), "etcpasswd");
        assert_eq!(sanitise_name("a/b").unwrap(), "ab");
        assert_eq!(sanitise_name("map\0name").unwrap(), "mapname");
        assert_eq!(sanitise_name("C:\\maps\\x").unwrap(), "Cmapsx");
        // And a name with nothing usable in it is not a name.
        assert!(matches!(sanitise_name(".."), Err(MapFault::Name)));
        assert!(matches!(sanitise_name("   "), Err(MapFault::Name)));
        assert!(matches!(sanitise_name("---"), Err(MapFault::Name)));
    }

    /// Long names are cut rather than refused, and cut to something that is still a name.
    #[test]
    fn a_name_is_capped_rather_than_rejected() {
        let long = "x".repeat(MAX_NAME * 3);
        let name = sanitise_name(&long).expect("a long name is still a name");
        assert_eq!(name.len(), MAX_NAME);
    }

    /// The ground under a point, between the samples, is what puts a spawn on a map that has just
    /// been switched — before there is a collider to cast a ray at.
    #[test]
    fn the_ground_under_a_point_is_read_between_the_samples() {
        let terrain = default_terrain();
        // On a sample, it is that sample exactly.
        for (x, z) in [(0.0, 0.0), (-85.0, 60.0), (12.0, -34.0)] {
            let grid = terrain.grid;
            let ix = ((x - grid.origin_x) / grid.spacing).round() as u32;
            let iz = ((z - grid.origin_z) / grid.spacing).round() as u32;
            let want = terrain.height_at(ix, iz);
            let got = terrain.height_over(x, z);
            assert!((got - want).abs() < 1e-3, "at {x},{z}: {got} rather than {want}");
        }
        // Between two, it is between their heights.
        let (low, high) = (terrain.height_over(-85.0, 60.0), terrain.height_over(-84.0, 60.0));
        let middle = terrain.height_over(-84.5, 60.0);
        assert!(
            middle >= low.min(high) - 1e-4 && middle <= low.max(high) + 1e-4,
            "{middle} is not between {low} and {high}",
        );
        // And past the rim it is the rim, not a panic and not a hole.
        let half = DEFAULT_EXTENT / 2.0;
        assert_eq!(terrain.height_over(-half - 50.0, 0.0), terrain.height_over(-half, 0.0));
        assert_eq!(terrain.height_over(0.0, half + 50.0), terrain.height_over(0.0, half));
    }

    /// A hill you slide off is scenery, and a ravine you walk out of is a ditch. The limit that
    /// decides which is which is [`WALKABLE_NORMAL_Y`], and the map has to fall on both sides of it
    /// deliberately rather than by luck — every hill climbable, every ravine wall not.
    ///
    /// Measured on the map as it stands: 35.0° at its steepest away from the ravines, 66.5° inside
    /// them, against a limit of 45.6°. Both margins are wide, and this test is what says so after
    /// somebody has moved a hill.
    ///
    /// The slope of a height field is read from its own samples rather than from the collider,
    /// because that is where a wrong number would be introduced.
    #[test]
    fn the_hills_are_climbable_and_the_ravines_are_not() {
        let terrain = default_terrain();
        let grid = terrain.grid;
        let limit = WALKABLE_NORMAL_Y.acos().to_degrees();
        let mut hills: f32 = 0.0;
        let mut walls: f32 = 0.0;
        for iz in 0..grid.nz - 1 {
            for ix in 0..grid.nx - 1 {
                let here = grid.world_of(ix, iz);
                let height = terrain.height_at(ix, iz);
                let dx = (terrain.height_at(ix + 1, iz) - height) / grid.spacing;
                let dz = (terrain.height_at(ix, iz + 1) - height) / grid.spacing;
                let steepness = (1.0f32 / (1.0 + dx * dx + dz * dz).sqrt()).acos().to_degrees();
                // Widened by a few metres, because the wall of a ravine is beside the line that
                // cut it, not on it.
                let in_a_ravine = RAVINES.iter().any(|ravine| {
                    let margin = ravine.width + 4.0;
                    to_segment_squared(here, ravine.from, ravine.to) < margin * margin
                });
                if in_a_ravine {
                    walls = walls.max(steepness);
                } else {
                    hills = hills.max(steepness);
                }
            }
        }
        assert!(hills < limit, "there is ground at {hills:.1}° to climb, and the limit is {limit:.1}°");
        assert!(walls > limit + 10.0, "the ravines only reach {walls:.1}°, which is walkable");
    }

    /// Nothing both machines run may reach for a function `libm` computes rather than the CPU.
    ///
    /// Read off the source, because the rule is about what is *written* and no runtime check can
    /// see it. Both files, and the second matters more than the first: the map generator has to
    /// agree between two machines that each build it once, and a brush has to agree between two
    /// machines applying a hundred strokes.
    ///
    /// Everything before `#[cfg(test)]` is scanned rather than a named span. The first version of
    /// this searched from `fn default_terrain` to the next occurrence of that same string, a
    /// boundary that held by accident and would have moved silently the day somebody wrote that
    /// name a third time.
    #[test]
    fn nothing_that_both_sides_run_reaches_for_a_transcendental() {
        for (file, source, landmark) in [
            ("terrain.rs", include_str!("terrain.rs"), "fn default_terrain"),
            ("sculpt.rs", include_str!("sculpt.rs"), "fn sculpt"),
        ] {
            let shipped = source.split("#[cfg(test)]").next().expect("a file");
            assert!(shipped.contains(landmark), "the split lost {landmark} in {file}");
            // Comments are dropped first, or the paragraph explaining why `mul_add` is forbidden
            // trips the check that forbids it — which is a delightful way to fail and a useless
            // one. This is a search for *calls*.
            let shipped: String = shipped
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            for forbidden in
                [".sqrt()", ".powf(", ".powi(", ".sin(", ".cos(", ".exp(", ".hypot(", "mul_add"]
            {
                assert!(!shipped.contains(forbidden), "{file} uses {forbidden} outside its tests");
            }
        }
    }

    /// The two numbers an author types, and the grid they mean.
    #[test]
    fn extent_and_spacing_give_the_sample_count_a_heightmap_tool_would() {
        let grid = Grid::new(512.0, 512.0, 1.0, -64.0, 64.0).expect("a canonical map");
        assert_eq!((grid.nx, grid.nz), (513, 513), "512 m at 1 m spacing is 513 grid points");
        assert_eq!(grid.extent_x(), 512.0, "and it says the extent back unchanged");
    }

    /// A map may be a strip. Nothing here assumes a square.
    #[test]
    fn the_two_axes_are_independent() {
        let grid = Grid::new(400.0, 100.0, 2.0, 0.0, 50.0).expect("a strip");
        assert_eq!((grid.nx, grid.nz), (201, 51));
        assert_eq!((grid.extent_x(), grid.extent_z()), (400.0, 100.0));
    }

    /// The canonical import sizes have to fit, which is the whole reason the axis cap is 2049 and
    /// not 2048.
    #[test]
    fn a_canonical_heightmap_size_is_not_rejected_by_one_sample() {
        for n in [513u32, 1025, 2049] {
            assert!(n <= MAX_SAMPLES_PER_AXIS, "{n} is a size heightmap tools export");
        }
        let grid = Grid { nx: 2049, nz: 2049, ..Grid::new(8.0, 8.0, 1.0, 0.0, 1.0).unwrap() };
        assert_eq!(
            grid.check(),
            Err(MapFault::TooManySamples),
            "2049 squared is 8.4 MB of heights and has to be refused by the total, not the axis"
        );
    }

    /// The cap that an extreme aspect ratio would otherwise walk through.
    #[test]
    fn a_long_thin_map_is_caught_by_the_total_and_not_by_either_axis() {
        let grid = Grid { nx: 2049, nz: 1200, ..Grid::new(8.0, 8.0, 1.0, 0.0, 1.0).unwrap() };
        assert!(
            grid.nx <= MAX_SAMPLES_PER_AXIS && grid.nz <= MAX_SAMPLES_PER_AXIS,
            "both axes are inside their own cap, which is the point",
        );
        assert_eq!(grid.check(), Err(MapFault::TooManySamples));
    }

    /// The exploit the caps exist for: a 10 km map at 5 cm spacing.
    #[test]
    fn the_denial_of_service_map_is_refused() {
        assert_eq!(Grid::new(10_000.0, 10_000.0, 0.05, -64.0, 64.0), Err(MapFault::Spacing));
        assert_eq!(Grid::new(10_000.0, 10_000.0, 1.0, -64.0, 64.0), Err(MapFault::TooLongAnAxis));
    }

    /// Nonsense in, refusal out — including the values that are not numbers at all.
    #[test]
    fn a_grid_that_is_not_a_grid_is_refused() {
        assert_eq!(Grid::new(100.0, 100.0, f32::NAN, 0.0, 1.0), Err(MapFault::Spacing));
        assert_eq!(Grid::new(f32::INFINITY, 100.0, 1.0, 0.0, 1.0), Err(MapFault::Extent));
        assert_eq!(Grid::new(0.0, 100.0, 1.0, 0.0, 1.0), Err(MapFault::Extent));
        assert_eq!(Grid::new(100.0, 100.0, 1.0, 5.0, 5.0), Err(MapFault::Range));
        assert_eq!(Grid::new(100.0, 100.0, 1.0, 0.0, f32::NAN), Err(MapFault::Range));
    }

    /// Every sample value survives the round trip through metres and back.
    ///
    /// Exhaustive, because there are only 65536 of them and "for all" is a stronger statement than
    /// any sample of them would be. This is the property a sculpt brush stands on: a stroke that
    /// reads a height, adds nothing and writes it back must change nothing at all.
    #[test]
    fn every_sample_survives_the_trip_through_metres() {
        let grid = Grid::new(64.0, 64.0, 1.0, -64.0, 64.0).expect("a grid");
        for sample in 0..=u16::MAX {
            let there_and_back = grid.quantise(grid.height(sample));
            assert_eq!(there_and_back, sample, "{sample} came back as {there_and_back}");
        }
    }

    /// What the quantisation actually costs, stated as a number rather than as a hope.
    #[test]
    fn a_128_metre_range_resolves_to_two_millimetres() {
        let grid = Grid::new(64.0, 64.0, 1.0, -64.0, 64.0).expect("a grid");
        let step = grid.height(1) - grid.height(0);
        assert!(step < 0.002, "one step is {:.4} mm", step * 1000.0);
    }

    /// Past either end a brush stops rather than wrapping. A mountain must not come out as a pit.
    #[test]
    fn pushing_past_the_range_saturates_rather_than_wrapping() {
        let grid = Grid::new(64.0, 64.0, 1.0, -64.0, 64.0).expect("a grid");
        assert_eq!(grid.quantise(1000.0), u16::MAX);
        assert_eq!(grid.quantise(-1000.0), 0);
        assert_eq!(grid.quantise(f32::NEG_INFINITY), 0);
    }

    /// A fresh map is flat at half height, so the first stroke works whichever way it goes.
    #[test]
    fn a_new_map_starts_halfway_up_its_own_range() {
        let terrain = Terrain::new(64.0, 64.0, 1.0, -64.0, 64.0).expect("a map");
        assert_eq!(terrain.heights.len(), terrain.grid.samples());
        assert!(
            terrain.heights.iter().all(|&h| h == terrain.grid.midpoint()),
            "a new map is not flat"
        );
        let y = terrain.height_at(0, 0);
        assert!(y.abs() < 0.002, "the midpoint of -64..64 came out at {y} rather than at 0");
        // Room to dig as well as to raise, which is the whole reason for the midpoint.
        assert!(y - terrain.grid.min_y > 63.0 && terrain.grid.max_y - y > 63.0);
    }

    /// Row-major, x fastest — the order the blob is in, and the one thing a reader cannot guess.
    #[test]
    fn the_samples_are_laid_out_with_x_running_fastest() {
        let grid = Grid::new(24.0, 16.0, 8.0, 0.0, 1.0).expect("a grid");
        assert_eq!((grid.nx, grid.nz), (4, 3));
        assert_eq!(grid.index(0, 0), 0);
        assert_eq!(grid.index(1, 0), 1, "the next sample along x is the next in the blob");
        assert_eq!(grid.index(0, 1), 4, "the next row is a whole nx away");
        assert_eq!(grid.index(3, 2), 11);
    }

    /// Samples land where the grid says, centred on the origin the level already uses.
    #[test]
    fn a_map_is_centred_on_the_world_origin() {
        let grid = Grid::new(100.0, 40.0, 5.0, 0.0, 1.0).expect("a grid");
        assert_eq!(grid.world_of(0, 0), Vec2::new(-50.0, -20.0));
        assert_eq!(grid.world_of(grid.nx - 1, grid.nz - 1), Vec2::new(50.0, 20.0));
        assert_eq!(grid.world_of(10, 4), Vec2::ZERO, "the middle sample is the origin");
    }

    /// And still centred when the extent is not a whole number of spacings, which is the case that
    /// would otherwise put the map half a spacing off the origin.
    #[test]
    fn a_map_that_does_not_divide_evenly_is_still_centred() {
        let grid = Grid::new(500.0, 500.0, 3.0, 0.0, 1.0).expect("a grid");
        assert_eq!(grid.nx, 168, "500 over 3 rounds to 167 spans");
        assert_eq!(grid.extent_x(), 501.0, "so the map is a metre wider than was asked for");
        let low = grid.world_of(0, 0);
        let high = grid.world_of(grid.nx - 1, grid.nz - 1);
        assert!(
            (low + high).length() < 1e-4,
            "it runs {low:?} to {high:?}, which is not centred on the origin",
        );
    }

    /// Heights out and back, byte for byte and sample for sample.
    #[test]
    fn the_heights_survive_the_blob() {
        let mut terrain = Terrain::new(24.0, 16.0, 8.0, -10.0, 10.0).expect("a map");
        for (index, sample) in terrain.heights.iter_mut().enumerate() {
            *sample = (index as u16).wrapping_mul(4919);
        }
        let blob = terrain.encode();
        assert_eq!(blob.len(), terrain.grid.samples() * 2);
        let back = Terrain::decode(terrain.grid, terrain.water_y, &blob).expect("it decodes");
        assert_eq!(back, terrain);
    }

    /// The two files can be separated, edited, or half-written. A mismatch that is not caught
    /// reads the height field off the end of itself.
    #[test]
    fn a_blob_that_does_not_match_its_manifest_is_refused() {
        let terrain = Terrain::new(24.0, 16.0, 8.0, -10.0, 10.0).expect("a map");
        let mut blob = terrain.encode();
        blob.pop();
        assert_eq!(
            Terrain::decode(terrain.grid, None, &blob),
            Err(MapFault::BlobSize { expected: 24, found: 23 })
        );
        assert_eq!(
            Terrain::decode(terrain.grid, None, &[]),
            Err(MapFault::BlobSize { expected: 24, found: 0 })
        );
    }

    /// A grid is checked before its length is believed, so a map over its caps never reaches an
    /// allocation.
    #[test]
    fn a_blob_is_not_even_measured_against_a_grid_that_failed_its_caps() {
        let grid = Grid { nx: 2049, nz: 2049, ..Grid::new(8.0, 8.0, 1.0, 0.0, 1.0).unwrap() };
        assert_eq!(Terrain::decode(grid, None, &[]), Err(MapFault::TooManySamples));
    }

    /// Whether two quaternions are the same turn.
    ///
    /// By the dot product rather than [`Quat::angle_between`], which goes through `acos` and loses
    /// half its digits next to zero: a round trip correct to a part in ten million reads there as
    /// three ten-thousandths of a radian of error. `q` and `−q` are the same rotation, hence the
    /// absolute value.
    fn same_turn(a: Quat, b: Quat) -> bool {
        a.dot(b).abs() > 1.0 - 1.0e-6
    }

    /// A yaw written by hand comes back as the rotation it means, in radians.
    #[test]
    fn a_yaw_in_the_file_is_degrees_and_a_quaternion_in_the_game() {
        let marker: Marker =
            serde_json::from_str(r#"{"kind":"vehicle","x":14,"z":-8,"yaw":90}"#).expect("it reads");
        assert_eq!(marker.kind, "vehicle");
        assert_eq!(marker.y, 0.0, "an absent offset is on the ground");
        let forward = marker.rotation * Vec3::NEG_Z;
        assert!(
            (forward - Vec3::NEG_X).length() < 1.0e-5,
            "90° should face -X, it faces {forward}",
        );
    }

    /// A rotation that is a yaw is written back as a yaw, whatever it arrived as.
    ///
    /// The rule that stops the shorthand being write-only. An "accept either form" reader paired
    /// with an "always write the general form" writer turns every hand-written `yaw: 45` into four
    /// numbers on the first save, and the file stops being readable exactly when the editor first
    /// touches it.
    #[test]
    fn a_marker_that_is_only_turned_is_written_as_a_yaw() {
        let marker = Marker {
            kind: "crate".into(),
            x: 1.0,
            z: 2.0,
            y: 0.0,
            rotation: Quat::from_rotation_y(0.75),
        };
        let text = serde_json::to_string(&marker).expect("it writes");
        assert!(text.contains("\"yaw\""), "written as {text}");
        assert!(!text.contains("rotation"), "written as {text}");
        assert!(!text.contains("\"y\""), "a zero offset should not be written: {text}");

        let back: Marker = serde_json::from_str(&text).expect("it reads back");
        assert!(same_turn(back.rotation, marker.rotation), "{back:?}");
    }

    /// A marker that is tilted keeps all four numbers.
    #[test]
    fn a_marker_that_is_tilted_is_written_in_full() {
        let marker = Marker {
            kind: "crate".into(),
            x: 0.0,
            z: 0.0,
            y: 0.5,
            rotation: Quat::from_rotation_x(0.3) * Quat::from_rotation_y(0.75),
        };
        let text = serde_json::to_string(&marker).expect("it writes");
        assert!(text.contains("rotation"), "written as {text}");
        assert!(!text.contains("yaw"), "written as {text}");

        let back: Marker = serde_json::from_str(&text).expect("it reads back");
        assert!(same_turn(back.rotation, marker.rotation), "{back:?}");
        assert_eq!(back.y, 0.5);
    }

    /// Both forms at once is refused rather than resolved.
    ///
    /// A precedence rule is a thing somebody has to remember correctly at two in the morning. The
    /// map that would need it is the one nobody looks at again.
    #[test]
    fn a_marker_may_not_give_both_a_yaw_and_a_rotation() {
        let text = r#"{"kind":"crate","x":0,"z":0,"yaw":45,"rotation":[0,0,0,1]}"#;
        let refused = serde_json::from_str::<Marker>(text).expect_err("it should be refused");
        assert!(
            refused.to_string().contains("never both"),
            "refused with the wrong reason: {refused}",
        );
    }

    /// A quaternion nobody normalised is normalised on the way in.
    ///
    /// It does not error when it is used — it scales and skews whatever it is applied to, which is
    /// a crate that is subtly the wrong size rather than a message anybody reads.
    #[test]
    fn a_sloppy_quaternion_is_made_a_rotation() {
        let marker: Marker =
            serde_json::from_str(r#"{"kind":"crate","x":0,"z":0,"rotation":[0,0.6,0,0.6]}"#)
                .expect("it reads");
        assert!((marker.rotation.length() - 1.0).abs() < 1.0e-6);
        assert_eq!(marker.check(default_terrain().grid), Ok(()));
    }

    /// The checks a marker off the wire or off a hand-edited file has to pass.
    #[test]
    fn a_marker_has_to_stand_on_the_map() {
        let grid = default_terrain().grid;
        let (low, high) = grid.bounds();
        let good = Marker {
            kind: "crate".into(),
            x: 0.0,
            z: 0.0,
            y: 0.0,
            rotation: Quat::IDENTITY,
        };
        assert_eq!(good.check(grid), Ok(()));

        let off_the_rim = Marker { x: high.x + 1.0, ..good.clone() };
        assert_eq!(off_the_rim.check(grid), Err(MapFault::Marker));
        let under_the_rim = Marker { z: low.y - 1.0, ..good.clone() };
        assert_eq!(under_the_rim.check(grid), Err(MapFault::Marker));
        let too_high = Marker { y: MAX_MARKER_Y + 0.1, ..good.clone() };
        assert_eq!(too_high.check(grid), Err(MapFault::Marker));
        // Both directions, which an absolute height would not need: a negative offset is a crate
        // half sunk into a slope, and a large one is a spawn under the map.
        let too_deep = Marker { y: -MAX_MARKER_Y - 0.1, ..good.clone() };
        assert_eq!(too_deep.check(grid), Err(MapFault::Marker));
        let nameless = Marker { kind: String::new(), ..good.clone() };
        assert_eq!(nameless.check(grid), Err(MapFault::Marker));
        let bent = Marker { rotation: Quat::from_xyzw(0.0, 2.0, 0.0, 0.0), ..good };
        assert_eq!(bent.check(grid), Err(MapFault::Marker));
    }

    /// A marker's height follows the ground rather than the world.
    ///
    /// The whole reason `y` is an offset: sculpt under a spawn and it comes up with the hill
    /// instead of ending inside it.
    #[test]
    fn a_marker_rides_the_ground_it_stands_on() {
        let mut terrain = default_terrain();
        let marker =
            Marker { kind: "crate".into(), x: 4.0, z: 4.0, y: 1.5, rotation: Quat::IDENTITY };
        let before = marker.where_it_stands(&terrain);
        assert!(
            (before.y - terrain.height_over(4.0, 4.0) - 1.5).abs() < 1.0e-4,
            "it should stand 1.5 m over the ground",
        );

        terrain
            .sculpt(&crate::sculpt::Stroke {
                at: Vec2::new(4.0, 4.0),
                radius: 8.0,
                brush: crate::sculpt::Brush::Lift { metres: 5.0 },
            })
            .expect("the ground moved");
        let after = marker.where_it_stands(&terrain);
        assert!(
            after.y > before.y + 4.0,
            "the ground rose 5 m and the marker went from {before} to {after}",
        );
        assert!(
            (after.y - terrain.height_over(4.0, 4.0) - 1.5).abs() < 1.0e-4,
            "it should still stand 1.5 m over the ground",
        );
    }

    /// Every default layer wins somewhere on the map everybody starts on.
    ///
    /// A rule set is only legible if you can see all of it, and a layer that never comes out on top
    /// is a rule nobody can check by looking — it would be indistinguishable from a typo in its own
    /// band. This walks the built-in map and asks which layer wins at each sample.
    #[test]
    fn every_default_layer_shows_somewhere_on_the_default_map() {
        let terrain = default_terrain();
        let layers = default_layers();
        let mut seen = vec![0usize; layers.len()];
        let grid = terrain.grid;
        // Every fourth sample in each direction: 16k probes rather than 263k, and there is nothing
        // on the map four metres wide.
        for iz in (1..grid.nz - 1).step_by(4) {
            for ix in (1..grid.nx - 1).step_by(4) {
                let here = grid.world_of(ix, iz);
                let y = terrain.height_over(here.x, here.y);
                // The normal the drawn mesh uses: central differences over the two neighbours.
                let east = terrain.height_at(ix + 1, iz);
                let west = terrain.height_at(ix - 1, iz);
                let north = terrain.height_at(ix, iz - 1);
                let south = terrain.height_at(ix, iz + 1);
                let normal = Vec3::new(west - east, 2.0 * grid.spacing, south - north).normalize();
                if let Some(index) = surface_of(&layers, normal.y, y, terrain.dip_at(ix, iz)) {
                    seen[index] += 1;
                }
            }
        }
        for (index, count) in seen.iter().enumerate() {
            assert!(
                *count > 0,
                "the layer {:?} never shows on the built-in map",
                layers[index].texture,
            );
        }
    }

    /// A hollow reads as a hollow, a ridge as a ridge, and a plane as neither.
    ///
    /// [`Terrain::dip_at`] is the one input to the look that is not a property of the point, so its
    /// sign is the whole rule: get it backwards and dirt goes on every hilltop.
    #[test]
    fn a_dip_is_measured_against_the_ground_around_it() {
        let flat = Terrain::new(128.0, 128.0, 1.0, -20.0, 20.0).unwrap();
        let middle = flat.grid.nx / 2;
        assert!(flat.dip_at(middle, middle).abs() < 1.0e-3, "level ground is not a hollow");

        // A bowl and a dome of the same size, built from the same shape with the sign flipped.
        // Fourteen metres across, so that the four samples `dip_at` reaches for at twelve are out
        // near the rim: a feature much wider than the span it is measured over is, correctly, not
        // measured as one.
        for (sign, what) in [(-1.0_f32, "a bowl"), (1.0, "a dome")] {
            let mut terrain = flat.clone();
            let grid = terrain.grid;
            for iz in 0..grid.nz {
                for ix in 0..grid.nx {
                    let here = grid.world_of(ix, iz);
                    let y = sign * 8.0 * falloff(here.length_squared(), 14.0);
                    let index = grid.index(ix, iz);
                    terrain.heights[index] = grid.quantise(y);
                }
            }
            let dip = terrain.dip_at(middle, middle);
            assert!(
                dip * -sign > 1.0,
                "the middle of {what} measured a dip of {dip:.2} m",
            );
        }
    }

    /// Dirt only ever lands somewhere that is genuinely lower than what surrounds it.
    ///
    /// The rule the middle layer exists for, checked against the map rather than against itself:
    /// every sample where `Ground048` wins is asked whether it really sits in a hollow. Before this
    /// the middle layer was a *slope* band, and it covered a fifth of the map — every hillside
    /// steeper than a gentle one, which is not a place dirt collects and did not look like one.
    #[test]
    fn dirt_only_lands_where_the_ground_is_lower_than_its_surroundings() {
        let terrain = default_terrain();
        let layers = default_layers();
        let grid = terrain.grid;
        let dirt = 1;
        let mut found = 0;
        for iz in (1..grid.nz - 1).step_by(4) {
            for ix in (1..grid.nx - 1).step_by(4) {
                let here = grid.world_of(ix, iz);
                let east = terrain.height_at(ix + 1, iz);
                let west = terrain.height_at(ix - 1, iz);
                let north = terrain.height_at(ix, iz - 1);
                let south = terrain.height_at(ix, iz + 1);
                let normal = Vec3::new(west - east, 2.0 * grid.spacing, south - north).normalize();
                let dip = terrain.dip_at(ix, iz);
                let y = terrain.height_over(here.x, here.y);
                if surface_of(&layers, normal.y, y, dip) == Some(dirt) {
                    found += 1;
                    assert!(
                        dip > 0.0,
                        "dirt won at {here} on ground {dip:.2} m below its surroundings",
                    );
                }
            }
        }
        assert!(found > 0, "the hollow rule never fires on the built-in map");
    }

    /// The rules cover everything, so no pixel falls through them.
    ///
    /// The shader paints a point outside every band bright magenta on purpose — a gap should look
    /// like a gap rather than like unlit ground. This is what says the default set has none, over
    /// every slope a surface can have, every height the map's own range allows, and every hollow
    /// and ridge from ten metres of either.
    #[test]
    fn the_default_rules_leave_no_gap() {
        let layers = default_layers();
        let mut slope = 0.0;
        while slope <= 90.0 {
            let mut y = DEFAULT_MIN_Y;
            while y <= DEFAULT_MAX_Y {
                let mut dip = -10.0;
                while dip <= 10.0 {
                    let total: f32 = layers.iter().map(|l| l.weight(slope, y, dip)).sum();
                    assert!(
                        total > 1.0e-3,
                        "no layer covers a {slope:.0}° surface at y = {y:.0}, dip {dip:.0} m",
                    );
                    dip += 0.5;
                }
                y += 1.0;
            }
            slope += 0.5;
        }
    }

    /// Two layers sharing an edge sum to one all the way across it, so the seam never dips.
    ///
    /// The property that the centred fade exists for. It survives the fade being a smoothstep
    /// because a smoothstep is symmetric about its middle — which is worth a test of its own
    /// rather than a claim, since it is the one thing that would break if the curve were ever
    /// swapped for one that is not.
    #[test]
    fn two_bands_sharing_an_edge_sum_to_one() {
        let low = Band { from: -100.0, to: 30.0, blend: 10.0 };
        let high = Band { from: 30.0, to: 100.0, blend: 10.0 };
        let mut x = 15.0;
        while x <= 45.0 {
            let total = low.weight(x) + high.weight(x);
            assert!((total - 1.0).abs() < 1.0e-5, "at {x} the two bands sum to {total}");
            x += 0.25;
        }
    }

    /// The manifest is the half that changes, so it has to survive a round trip through text.
    #[test]
    fn the_manifest_survives_json() {
        let mut terrain = Terrain::new(64.0, 64.0, 1.0, -64.0, 64.0).expect("a map");
        terrain.water_y = Some(-3.5);
        let mut manifest = terrain.manifest();
        manifest.layers = default_layers();
        manifest.markers.push(Marker {
            kind: "crate".into(),
            x: 1.5,
            z: -2.0,
            y: 0.0,
            rotation: Quat::from_rotation_y(1.57),
        });
        // One that is tilted as well, so the round trip covers both forms the file may take.
        manifest.markers.push(Marker {
            kind: "vehicle".into(),
            x: -4.0,
            z: 6.0,
            y: 0.25,
            rotation: Quat::from_rotation_x(0.2) * Quat::from_rotation_y(1.0),
        });

        let text = serde_json::to_string_pretty(&manifest).expect("it serialises");
        let back: Manifest = serde_json::from_str(&text).expect("it deserialises");
        assert_eq!(back, manifest);
        assert!(back.check().is_ok());
    }

    /// An added field must cost nothing, which is the whole argument for JSON over a binary
    /// manifest. A map written before layers and markers existed still loads.
    #[test]
    fn a_manifest_from_before_the_new_fields_still_loads() {
        let text = r#"{
            "version": 1,
            "grid": { "nx": 65, "nz": 65, "spacing": 1.0,
                      "origin_x": -32.0, "origin_z": -32.0, "min_y": -64.0, "max_y": 64.0 }
        }"#;
        let manifest: Manifest = serde_json::from_str(text).expect("an older map still loads");
        assert_eq!(manifest.grid.nx, 65);
        assert_eq!(manifest.water_y, None, "a map with no water stores nothing");
        assert!(manifest.layers.is_empty() && manifest.markers.is_empty());
        assert!(manifest.check().is_ok());
    }

    /// A manifest from a newer build is refused rather than half-read.
    #[test]
    fn a_map_from_a_later_version_is_refused() {
        let terrain = Terrain::new(64.0, 64.0, 1.0, -64.0, 64.0).expect("a map");
        let mut manifest = terrain.manifest();
        manifest.version = VERSION + 1;
        assert_eq!(manifest.check(), Err(MapFault::Version(VERSION + 1)));
    }

    /// A hand-edited manifest faces the same caps a created map does. This is the path that would
    /// otherwise get the check forgotten on it.
    #[test]
    fn a_manifest_gets_the_same_caps_a_new_map_does() {
        let text = r#"{
            "version": 1,
            "grid": { "nx": 40000, "nz": 40000, "spacing": 0.05,
                      "origin_x": 0.0, "origin_z": 0.0, "min_y": -64.0, "max_y": 64.0 }
        }"#;
        let manifest: Manifest = serde_json::from_str(text).expect("it parses");
        assert_eq!(manifest.check(), Err(MapFault::Spacing));
    }

    /// The caps have to be the ones the wire budget implies, or the menu greys out against one
    /// number while the server enforces another.
    #[test]
    fn the_sample_cap_is_the_baseline_budget_and_nothing_else() {
        assert_eq!(MAX_SAMPLES as usize * 2, MAX_BASELINE_BYTES);
        let kilometre = Grid::new(1024.0, 1024.0, 1.0, -64.0, 64.0).expect("a kilometre fits");
        assert_eq!((kilometre.nx, kilometre.nz), (1025, 1025));
        assert!(kilometre.samples() * 2 < MAX_BASELINE_BYTES);
    }
}
