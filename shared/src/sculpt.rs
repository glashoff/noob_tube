//! Sculpting: the four brushes, and the gesture that carries one over the wire.
//!
//! **A stroke travels as an instruction, not as a result.** `TerrainEdit` says where the brush was,
//! how wide, and what it was doing; every machine recomputes the same samples from it. That is the
//! principle [`simulation`](crate::simulation) already rests on, one level up — send the input, not
//! the outcome, and keep one implementation of the step that turns one into the other — and here it
//! is also the difference between forty bytes and a megabyte.
//!
//! It only works if every machine lands on exactly the same numbers, so two rules hold throughout:
//!
//! **No transcendental functions.** The brushes use `+ - * /`, `round`, `min`/`max` and
//! comparisons, every one of which IEEE-754 specifies exactly and every machine computes
//! identically. Radial falloff is therefore a polynomial in the **squared** distance and never
//! takes a square root: `sqrt`, `powf`, `exp`, `sin` and `hypot` go through `libm`, which is not
//! required to be correctly rounded and may differ in the last ulp between platforms. `mul_add` is
//! out too — it is a *different* result from `a * b + c`, deliberately, and whether it lowers to
//! one instruction or two is a property of the target.
//!
//! **The lattice is the checkpoint.** Every stroke rounds to the map's own `u16` quantisation as it
//! writes, so a hundred small strokes cannot accumulate apart between two machines the way a
//! hundred `f32` additions would. That is what the quantisation is *for*; halving the storage is a
//! side effect.
//!
//! ### When a stroke lands
//!
//! Not when it is sent. [`Level`](crate::physics::Level) reads Avian's *current* spatial state and
//! there is no seam in it where a rollback replay could be handed historical terrain — so an edit
//! applied inside the rollback window would have every replayed tick, including the ones from
//! before the edit, walked on the new ground.
//!
//! So the server stamps every edit with `commit_tick + edit_delay_ticks` and both sides apply it
//! there. Choosing the tick deliberately is the same move
//! [`lag_compensation`](crate::lag_compensation) already makes for shots, and the cost is that a
//! sculptor waits that long to see their own stroke — which is why the margin is chosen and not
//! merely made large. See [`edit_delay_ticks`](crate::tuning::NetConfig::edit_delay_ticks): it was
//! `max_predicted_ticks` at first, which puts the ceiling on how far a client may *ever* predict
//! into a wait paid on every stroke, and that was a second and a half.

use avian3d::prelude::{LinearVelocity, Position};
use bevy::ecs::query::QueryFilter;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use lightyear::prelude::{LocalTimeline, Tick};

use crate::movement::SKIN;
use crate::player::PlayerState;
use crate::vehicle::VehicleKind;
use crate::terrain::{Grid, Ground, MapFault, Terrain, falloff, to_segment_squared};

/// The widest a single stroke may be, in metres.
///
/// A cap because the cost of a stroke is its area, and the area is quadratic in this. It is also
/// what a brush can usefully be: past a certain size the tool is "make a hill", which is a
/// different gesture and would want a different name.
pub const MAX_RADIUS: f32 = 64.0;

/// The most one stroke may move the ground at its centre, in metres.
pub const MAX_LIFT: f32 = 16.0;

/// How many samples a client may write per second, and how many it may bank.
///
/// A token bucket rather than a delay between strokes, because sculpting is *held*: the natural
/// gesture is a stream of overlapping strokes, and anything that refuses the second one in a
/// hundred milliseconds refuses sculpting itself. What it limits is area, which is the thing that
/// costs — and it is charged on an **upper bound worked out before anything is applied**.
/// Undercharging would let a client buy a bigger stroke than it pays for, and since the commit is
/// broadcast before it is applied, an oversized stroke stalls every machine in the game rather than
/// only the sender's.
pub const SAMPLES_PER_SECOND: f32 = 400_000.0;
pub const SAMPLE_BURST: f32 = 800_000.0;

/// What a stroke does to the ground under it.
///
/// In the order they earn their place. **Flatten is not optional**: built structures have flat
/// footprints and unsculpted ground does not, so without it every building gets a gap under one
/// corner and buries another.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum Brush {
    /// Level the ground to a height, which the sculptor picks off the ground with an eyedropper.
    ///
    /// Eased by the falloff like the rest, so one pass leaves a soft edge and holding the brush
    /// converges on flat — which is what a held brush is for, and why nothing here is idempotent
    /// except a stroke that has already arrived.
    Flatten { height: f32 },
    /// Raise, or lower when it is negative, with the falloff giving the soft edge.
    Lift { metres: f32 },
    /// Pull every sample toward the mean of its four neighbours.
    Smooth { amount: f32 },
    /// A straight run between two points, with the height interpolated along it.
    ///
    /// The other end and both heights, because a ramp is the one brush whose shape is not a disc:
    /// it is the capsule of `radius` around the segment from the stroke's own point to `to`.
    Ramp { to: Vec2, from_y: f32, to_y: f32 },
}

/// One stroke, as a sculptor asks for it.
///
/// What a client sends. It carries no tick, and that is the point: *when* a stroke lands is the
/// server's to decide, and a type that let a client name the tick would be a type somebody has to
/// remember to overwrite.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    /// Where the brush is, in world x and z.
    pub at: Vec2,
    pub radius: f32,
    pub brush: Brush,
}

/// A stroke that has been accepted, with the tick every machine applies it on.
///
/// What the server broadcasts. See the module note for why the tick is not simply "now".
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct TerrainEdit {
    pub stroke: Stroke,
    pub tick: u32,
}

/// The box of samples a stroke wrote, inclusive on both ends.
///
/// What a tile needs to know whether it has to rebuild. Inclusive rather than exclusive because
/// a stroke that touches exactly one sample is a real stroke and an exclusive range would have to
/// say so with a `+ 1` at every call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch {
    pub ix0: u32,
    pub ix1: u32,
    pub iz0: u32,
    pub iz1: u32,
}

impl Patch {
    pub fn samples(&self) -> u64 {
        (self.ix1 - self.ix0 + 1) as u64 * (self.iz1 - self.iz0 + 1) as u64
    }

    /// Where it is in the world, as the two corners of its box.
    pub fn bounds(&self, grid: &Grid) -> (Vec2, Vec2) {
        (grid.world_of(self.ix0, self.iz0), grid.world_of(self.ix1, self.iz1))
    }
}

impl Stroke {
    /// Whether this is a stroke a server will apply, before anything is allocated or charged for
    /// it.
    ///
    /// Every number is checked for being finite first. A `NaN` radius passes every comparison
    /// written the obvious way — `radius > MAX_RADIUS` is false for `NaN` — and would then make the
    /// sample box `NaN` and the cast that turns it into an index undefined-ish rather than merely
    /// wrong.
    pub fn check(&self, grid: &Grid) -> Result<(), MapFault> {
        if !self.at.is_finite() || !self.radius.is_finite() {
            return Err(MapFault::Stroke);
        }
        // Only now, with the radius known to be a number, are these comparisons worth anything:
        // `radius > MAX_RADIUS` is *false* for a `NaN`, so the obvious order lets one through.
        if self.radius <= 0.0 || self.radius > MAX_RADIUS {
            return Err(MapFault::Stroke);
        }
        let sane = match self.brush {
            Brush::Flatten { height } => height.is_finite(),
            Brush::Lift { metres } => metres.is_finite() && metres.abs() <= MAX_LIFT,
            Brush::Smooth { amount } => (0.0..=1.0).contains(&amount),
            Brush::Ramp { to, from_y, to_y } => {
                to.is_finite() && from_y.is_finite() && to_y.is_finite()
            }
        };
        if !sane {
            return Err(MapFault::Stroke);
        }
        // Somewhere the map reaches, give or take a brush. A stroke that misses the map entirely is
        // not an attack, but it is not a stroke either, and letting it through means a client can
        // spend its whole budget on nothing.
        let far = self.radius + grid.spacing;
        let inside = |point: Vec2| {
            point.x >= grid.origin_x - far
                && point.x <= grid.origin_x + grid.extent_x() + far
                && point.y >= grid.origin_z - far
                && point.y <= grid.origin_z + grid.extent_z() + far
        };
        let reaches = inside(self.at)
            || matches!(self.brush, Brush::Ramp { to, .. } if inside(to));
        if !reaches {
            return Err(MapFault::Stroke);
        }
        Ok(())
    }

    /// The most samples this stroke could write, worked out without writing any.
    ///
    /// What the token bucket is charged. It is the whole bounding box rather than the disc inside
    /// it, and rounded outward on both ends of each axis: an upper bound is the only kind of
    /// estimate that is safe to charge, since the alternative is a client buying more area than it
    /// paid for.
    pub fn cost(&self, grid: &Grid) -> u64 {
        self.box_of(grid).map(|patch| patch.samples()).unwrap_or(0)
    }

    /// The samples this stroke can reach, clamped to the map. `None` when it reaches none of them.
    fn box_of(&self, grid: &Grid) -> Option<Patch> {
        let (mut low, mut high) = (self.at, self.at);
        if let Brush::Ramp { to, .. } = self.brush {
            low = low.min(to);
            high = high.max(to);
        }
        low -= Vec2::splat(self.radius);
        high += Vec2::splat(self.radius);

        // Outward on both ends, so a stroke never falls between two samples and writes neither.
        let first = |world: f32, origin: f32| ((world - origin) / grid.spacing).floor();
        let last = |world: f32, origin: f32| ((world - origin) / grid.spacing).ceil();
        let clamp = |value: f32, n: u32| value.clamp(0.0, (n - 1) as f32) as u32;
        let (ix0, ix1) = (
            clamp(first(low.x, grid.origin_x), grid.nx),
            clamp(last(high.x, grid.origin_x), grid.nx),
        );
        let (iz0, iz1) = (
            clamp(first(low.y, grid.origin_z), grid.nz),
            clamp(last(high.y, grid.origin_z), grid.nz),
        );
        // A stroke entirely off one side clamps to the same edge sample at both ends, which is a
        // box of one sample that the falloff then finds nothing to do in. Cheap and harmless.
        Some(Patch { ix0, ix1, iz0, iz1 })
    }

    /// How much of this brush applies at a point, from 0 at the rim to 1 in the middle.
    fn weight(&self, here: Vec2) -> f32 {
        let squared = match self.brush {
            Brush::Ramp { to, .. } => to_segment_squared(here, self.at, to),
            _ => (here - self.at).length_squared(),
        };
        falloff(squared, self.radius)
    }
}

impl Terrain {
    /// Applies one stroke, and says which samples it wrote.
    ///
    /// The only place the ground is changed by anything but a map switch, and it is deliberately
    /// one function on both sides — the server runs it to be the authority and every client runs it
    /// to keep up, and a second copy of it is precisely how the two would stop agreeing.
    ///
    /// The stroke is assumed to have passed [`TerrainEdit::check`] already: this is the hot path,
    /// and re-checking here would mean the answer to "is this stroke allowed" living in two places.
    pub fn sculpt(&mut self, stroke: &Stroke) -> Option<Patch> {
        let grid = self.grid;
        let patch = stroke.box_of(&grid)?;

        // Smooth reads its neighbours, so it needs a snapshot: reading while writing in place makes
        // the answer depend on iteration order, and a sample would be smoothed against neighbours
        // that had already been smoothed. Only the box is copied — samples outside it are read
        // live, which is correct precisely because nothing outside it is written.
        let before = matches!(stroke.brush, Brush::Smooth { .. }).then(|| self.heights.clone());

        for iz in patch.iz0..=patch.iz1 {
            for ix in patch.ix0..=patch.ix1 {
                let here = grid.world_of(ix, iz);
                let weight = stroke.weight(here);
                if weight <= 0.0 {
                    continue;
                }
                let index = grid.index(ix, iz);
                let height = grid.height(self.heights[index]);
                let wanted = match stroke.brush {
                    Brush::Flatten { height: to } => height + (to - height) * weight,
                    Brush::Lift { metres } => height + metres * weight,
                    Brush::Smooth { amount } => {
                        let field = before.as_ref().unwrap_or(&self.heights);
                        let mean = self.neighbour_mean(field, ix, iz);
                        height + (mean - height) * amount * weight
                    }
                    Brush::Ramp { to, from_y, to_y } => {
                        let along = to - stroke.at;
                        let length_squared = along.length_squared();
                        let t = if length_squared <= 0.0 {
                            0.0
                        } else {
                            ((here - stroke.at).dot(along) / length_squared).clamp(0.0, 1.0)
                        };
                        let target = from_y + (to_y - from_y) * t;
                        height + (target - height) * weight
                    }
                };
                self.heights[index] = grid.quantise(wanted);
            }
        }
        Some(patch)
    }

    /// The mean of the four orthogonal neighbours, with the rim reading itself.
    ///
    /// Written as one sum and one multiply in a fixed order, which is what makes it the same number
    /// on every machine — float addition is not associative, so the order is part of the result.
    fn neighbour_mean(&self, field: &[u16], ix: u32, iz: u32) -> f32 {
        let grid = self.grid;
        let at = |ix: u32, iz: u32| grid.height(field[grid.index(ix, iz)]);
        let west = at(ix.saturating_sub(1), iz);
        let east = at((ix + 1).min(grid.nx - 1), iz);
        let south = at(ix, iz.saturating_sub(1));
        let north = at(ix, (iz + 1).min(grid.nz - 1));
        (west + east + south + north) * 0.25
    }
}

/// PreUpdate: takes whatever was parked on ground that has just risen up with it.
///
/// The same rescue as [`lift_with_the_ground`] and for the same reason, over the bodies Avian
/// simulates rather than the ones the movement step does. A height field has **no underside**: a
/// chassis that ends a tick below one has nothing to come back from, and a stroke that lifts the
/// ground six metres puts it there in a single tick. Measured before this existed — a parked buggy
/// under one brush stroke was 25 m below the surface two seconds later and still going.
///
/// Four corners rather than the centre, because a vehicle is four metres long: a hillside raised
/// under one end leaves the middle clear and buries the nose. They are the corners of the nominal
/// box in world axes rather than the rotated hull, which is an approximation and an honest one —
/// this is a rescue from a hole with no bottom, not a resting pose. The suspension puts it down
/// properly over the next few ticks.
pub fn lift_bodies_with_the_ground<F: QueryFilter + 'static>(
    ground: Res<Ground>,
    mut patched: MessageReader<GroundPatched>,
    mut bodies: Query<(&VehicleKind, &mut Position, &mut LinearVelocity), F>,
) {
    for GroundPatched(patch) in patched.read() {
        let (low, high) = patch.bounds(&ground.0.grid);
        for (kind, mut position, mut velocity) in bodies.iter_mut() {
            let at = position.0;
            let spec = kind.spec();
            // Its own footprint, grown by the patch: a stroke that lifted the ground beside a
            // vehicle still lifts the ground under its front wheel.
            let reach = spec.half_extents.x.max(spec.half_extents.z);
            if at.x < low.x - reach
                || at.x > high.x + reach
                || at.z < low.y - reach
                || at.z > high.y + reach
            {
                continue;
            }
            let mut surface = f32::MIN;
            for (dx, dz) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let corner = at + Vec3::new(dx * spec.half_extents.x, 0.0, dz * spec.half_extents.z);
                surface = surface.max(ground.0.height_over(corner.x, corner.z));
            }
            let rest = surface + spec.ride_height();
            if at.y < rest {
                position.0.y = rest;
                // Whatever downward speed it had belonged to resting on the old ground.
                velocity.0.y = velocity.0.y.max(0.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::default_terrain;

    fn flat_map() -> Terrain {
        Terrain::new(64.0, 64.0, 1.0, -32.0, 32.0).expect("a map within the caps")
    }

    fn stroke(brush: Brush) -> Stroke {
        Stroke { at: Vec2::ZERO, radius: 8.0, brush }
    }

    /// Raising puts the most in the middle, less at the edge, and nothing at all outside.
    #[test]
    fn a_lift_is_strongest_where_the_brush_is() {
        let mut map = flat_map();
        let before = map.height_over(0.0, 0.0);
        map.sculpt(&stroke(Brush::Lift { metres: 4.0 })).expect("it wrote something");

        let middle = map.height_over(0.0, 0.0) - before;
        let halfway = map.height_over(4.0, 0.0) - before;
        let outside = map.height_over(12.0, 0.0) - before;
        assert!((middle - 4.0).abs() < 0.01, "the middle rose {middle:.3} m, not 4");
        assert!(halfway > 0.5 && halfway < middle, "the edge rose {halfway:.3} m");
        assert!(outside.abs() < 1e-4, "ground outside the brush moved {outside:.4} m");
    }

    /// And lowering is the same brush with the sign turned round, not a second one.
    #[test]
    fn lowering_is_lifting_the_other_way() {
        let (mut up, mut down) = (flat_map(), flat_map());
        up.sculpt(&stroke(Brush::Lift { metres: 3.0 }));
        down.sculpt(&stroke(Brush::Lift { metres: -3.0 }));
        let flat = flat_map().height_over(0.0, 0.0);
        let risen = up.height_over(2.0, 1.0) - flat;
        let sunk = flat - down.height_over(2.0, 1.0);
        assert!((risen - sunk).abs() < 0.01, "up {risen:.3} m against down {sunk:.3} m");
    }

    /// Flatten levels what it covers to the height it was given, which is the property every
    /// building on a map depends on.
    #[test]
    fn flatten_levels_the_ground_under_it() {
        let mut map = default_terrain();
        // On the flank of a hill, where there is something to level.
        let at = Vec2::new(-85.0, 60.0);
        let edit = Stroke { at, radius: 10.0, brush: Brush::Flatten { height: 7.5 } };
        map.sculpt(&edit).expect("it wrote something");
        assert!((map.height_over(at.x, at.y) - 7.5).abs() < 0.01, "the middle is not level");
        // Two metres out is still well inside the brush and still nearly level.
        let near = map.height_over(at.x + 2.0, at.y);
        assert!((near - 7.5).abs() < 0.6, "two metres out sits at {near:.2} rather than 7.5");
    }

    /// Smooth pulls a spike down toward what is around it, and does it from a snapshot.
    ///
    /// The snapshot is what this really tests: without it a sample would be smoothed against
    /// neighbours that had already been smoothed this stroke, so the result would depend on which
    /// way the loops happen to run. Symmetry is the visible consequence — the two sides of a spike
    /// are mirror images, and only stay so if neither was written before the other was read.
    #[test]
    fn smooth_pulls_a_spike_down_symmetrically() {
        let mut map = flat_map();
        let spike = Stroke {
            at: Vec2::ZERO,
            radius: 2.0,
            brush: Brush::Lift { metres: 8.0 } };
        map.sculpt(&spike).expect("the spike");
        let peak = map.height_over(0.0, 0.0);

        map.sculpt(&stroke(Brush::Smooth { amount: 1.0 })).expect("the smoothing");
        assert!(map.height_over(0.0, 0.0) < peak, "the spike did not come down");
        let (west, east) = (map.height_over(-3.0, 0.0), map.height_over(3.0, 0.0));
        let (south, north) = (map.height_over(0.0, -3.0), map.height_over(0.0, 3.0));
        assert!((west - east).abs() < 1e-3, "{west:.4} west against {east:.4} east");
        assert!((south - north).abs() < 1e-3, "{south:.4} south against {north:.4} north");
    }

    /// A ramp runs between its two ends and takes their heights with it.
    #[test]
    fn a_ramp_climbs_from_one_end_to_the_other() {
        let mut map = flat_map();
        let edit = Stroke {
            at: Vec2::new(-10.0, 0.0),
            radius: 3.0,
            brush: Brush::Ramp { to: Vec2::new(10.0, 0.0), from_y: 0.0, to_y: 6.0 } };
        map.sculpt(&edit).expect("it wrote something");
        let low = map.height_over(-10.0, 0.0);
        let middle = map.height_over(0.0, 0.0);
        let high = map.height_over(10.0, 0.0);
        assert!(low < 0.2, "the low end is at {low:.2}");
        assert!((middle - 3.0).abs() < 0.3, "the middle is at {middle:.2}, not 3");
        assert!((high - 6.0).abs() < 0.3, "the high end is at {high:.2}, not 6");
        // And it is a capsule, not a box: well off to the side is untouched.
        let untouched = flat_map().height_over(0.0, 8.0);
        let beside = map.height_over(0.0, 8.0);
        assert!((beside - untouched).abs() < 1e-4, "the ground beside the ramp moved");
    }

    /// The same stroke twice on the same ground gives the same field, bit for bit.
    ///
    /// This is the whole reason a stroke may travel as a gesture: two machines run this and must
    /// land on identical `u16`s, not merely on heights that look alike.
    #[test]
    fn the_same_stroke_gives_the_same_field() {
        for brush in [
            Brush::Lift { metres: 2.5 },
            Brush::Flatten { height: -3.25 },
            Brush::Smooth { amount: 0.7 },
            Brush::Ramp { to: Vec2::new(7.0, -4.0), from_y: 1.0, to_y: -2.0 },
        ] {
            let mut once = default_terrain();
            let mut twice = default_terrain();
            let edit = Stroke {
            at: Vec2::new(-85.0, 60.0), radius: 12.0, brush };
            once.sculpt(&edit);
            twice.sculpt(&edit);
            assert_eq!(once.heights, twice.heights, "{brush:?} was not reproducible");
        }
    }

    /// Every stroke leaves the field on the map's own lattice, and that is the property the whole
    /// gesture scheme rests on.
    ///
    /// It is not that a stroke is idempotent — an eased brush applied twice moves the ground twice,
    /// which is what a held brush is *for*. It is that the rounding happens on every stroke rather
    /// than at the end, so two machines running the same hundred strokes cannot drift apart the way
    /// a hundred `f32` additions would.
    #[test]
    fn every_stroke_leaves_the_field_on_its_lattice() {
        let mut map = default_terrain();
        for brush in [
            Brush::Lift { metres: 2.5 },
            Brush::Smooth { amount: 0.4 },
            Brush::Flatten { height: -3.25 },
        ] {
            map.sculpt(&Stroke {
            at: Vec2::new(-85.0, 60.0), radius: 12.0, brush });
        }
        let grid = map.grid;
        for (index, &sample) in map.heights.iter().enumerate() {
            assert_eq!(
                grid.quantise(grid.height(sample)),
                sample,
                "sample {index} is off the lattice",
            );
        }
    }

    /// And where a brush has already finished its work, more of it changes nothing.
    ///
    /// At the very middle the falloff is 1, so flatten puts the sample exactly on the target's
    /// lattice point — and `height` being the exact inverse of `quantise` there is what makes the
    /// next stroke a no-op rather than a rounding error.
    #[test]
    fn a_finished_stroke_has_nothing_left_to_do() {
        let mut map = default_terrain();
        let at = Vec2::new(-85.0, 60.0);
        let edit = Stroke { at, radius: 12.0, brush: Brush::Flatten { height: 4.0 } };
        map.sculpt(&edit);
        let grid = map.grid;
        let middle = grid.index(
            ((at.x - grid.origin_x) / grid.spacing) as u32,
            ((at.y - grid.origin_z) / grid.spacing) as u32,
        );
        let settled = map.heights[middle];
        assert_eq!(settled, grid.quantise(4.0), "the middle did not reach the target");
        for _ in 0..20 {
            map.sculpt(&edit);
        }
        assert_eq!(map.heights[middle], settled, "the middle crept under twenty more strokes");
    }

    /// Ground that rises takes whoever is standing on it up with it.
    ///
    /// Found by playing rather than by reading: one stroke of a one-metre brush under a standing
    /// player buried them, because the ground probe reaches 12 cm below the feet and the new
    /// surface was above them — so they read as airborne inside solid ground and the sweep pushed
    /// them out through the bottom. Measured: they fell past −110 m and kept going.
    #[test]
    fn ground_that_rises_takes_a_player_with_it() {
        use crate::player::PlayerState;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.add_message::<GroundPatched>();
        app.insert_resource(Ground(flat_map()));
        let standing = app.world_mut().spawn(PlayerState::default()).id();
        // Well outside the brush, and it must not move.
        let bystander = app
            .world_mut()
            .spawn(PlayerState { position: Vec3::new(28.0, 0.0, 0.0), ..PlayerState::default() })
            .id();

        let stroke = stroke(Brush::Lift { metres: 4.0 });
        let patch = {
            let mut ground = app.world_mut().resource_mut::<Ground>();
            ground.0.sculpt(&stroke).expect("the stroke wrote something")
        };
        app.world_mut().write_message(GroundPatched(patch));
        app.world_mut().run_system_once(lift_with_the_ground::<()>).expect("the lift");

        let lifted = app.world().get::<PlayerState>(standing).expect("a player").position.y;
        assert!((lifted - 4.0).abs() < 0.2, "the player is at {lifted:.2}, not on the new ground");
        let aside = app.world().get::<PlayerState>(bystander).expect("a player").position.y;
        assert_eq!(aside, 0.0, "somebody outside the stroke was moved");
    }

    /// And ground that is lowered leaves them in the air, which is what a hole is for.
    #[test]
    fn ground_that_falls_leaves_a_player_standing_in_the_air() {
        use crate::player::PlayerState;
        use bevy::ecs::system::RunSystemOnce;

        let mut app = App::new();
        app.add_message::<GroundPatched>();
        app.insert_resource(Ground(flat_map()));
        let standing = app.world_mut().spawn(PlayerState::default()).id();

        let patch = {
            let mut ground = app.world_mut().resource_mut::<Ground>();
            ground.0.sculpt(&stroke(Brush::Lift { metres: -4.0 })).expect("the stroke")
        };
        app.world_mut().write_message(GroundPatched(patch));
        app.world_mut().run_system_once(lift_with_the_ground::<()>).expect("the lift");

        let after = app.world().get::<PlayerState>(standing).expect("a player").position.y;
        assert_eq!(after, 0.0, "the player was put down into the hole at {after}");
    }

    /// What a stroke is charged is an upper bound on what it writes, never less.
    #[test]
    fn a_stroke_is_charged_for_at_least_what_it_writes() {
        let grid = flat_map().grid;
        for radius in [0.5f32, 1.0, 3.0, 8.0, 30.0] {
            let edit = Stroke {
            at: Vec2::new(1.5, -2.5),
                radius,
                brush: Brush::Lift { metres: 1.0 } };
            let mut map = flat_map();
            let written = map.sculpt(&edit).expect("a patch").samples();
            assert!(
                edit.cost(&grid) >= written,
                "a radius of {radius} was charged {} for {written} samples",
                edit.cost(&grid),
            );
        }
    }

    /// Every way of asking for something silly is refused before anything is allocated for it.
    #[test]
    fn a_stroke_that_is_not_one_is_refused() {
        let grid = flat_map().grid;
        let lift = Brush::Lift { metres: 1.0 };
        let bad = [
            Stroke {
            at: Vec2::ZERO, radius: f32::NAN, brush: lift },
            Stroke {
            at: Vec2::ZERO, radius: 0.0, brush: lift },
            Stroke {
            at: Vec2::ZERO, radius: -4.0, brush: lift },
            Stroke {
            at: Vec2::ZERO, radius: MAX_RADIUS + 1.0, brush: lift },
            Stroke {
            at: Vec2::new(f32::INFINITY, 0.0), radius: 4.0, brush: lift },
            Stroke {
            at: Vec2::ZERO,
                radius: 4.0,
                brush: Brush::Lift { metres: MAX_LIFT + 1.0 } },
            Stroke {
            at: Vec2::ZERO,
                radius: 4.0,
                brush: Brush::Flatten { height: f32::NAN } },
            Stroke {
            at: Vec2::ZERO,
                radius: 4.0,
                brush: Brush::Smooth { amount: 4.0 } },
            // A long way off the map, which is not an attack but is not a stroke either.
            Stroke {
            at: Vec2::splat(9_000.0), radius: 4.0, brush: lift },
        ];
        for edit in bad {
            assert!(edit.check(&grid).is_err(), "{edit:?} was accepted");
        }
        assert!(stroke(lift).check(&grid).is_ok(), "an ordinary stroke was refused");
    }
}

/// Edits that have been accepted but whose tick has not come.
///
/// One list on both sides, in the order the server put them in. Ordered delivery is what makes that
/// order meaningful: edits do not commute — raise-then-smooth and smooth-then-raise are different
/// ground — so the queue is drained in the order it was filled and never sorted by tick.
#[derive(Resource, Default)]
pub struct PendingEdits(pub Vec<TerrainEdit>);

/// Says the ground has moved under a patch of samples.
///
/// A message rather than a resource, because two very different things want to know and neither is
/// the other's business: the collider rebuilds its tiles, and a client rebuilds their meshes. Each
/// reads the same message independently, which is what a message is for.
#[derive(Message, Clone, Copy, Debug)]
pub struct GroundPatched(pub Patch);

/// PreUpdate: applies every edit whose tick has come, and says which ground moved.
///
/// **Before `FixedMain`**, so the tick that runs this frame runs on the ground the edit describes
/// rather than a frame behind it. The tick it compares against is the one the simulation is about
/// to step, which on a client is its predicted tick and on the server is the present — so a client
/// applies an edit slightly ahead in wall-clock time, exactly as it predicts everything else.
///
/// An edit whose tick has already passed is applied at once. That is not a special case for lag: it
/// is what a client does with an edit that was in flight while it was joining, and doing anything
/// else would leave it on ground nobody else is standing on.
/// Gated on there being a map at all, which on a client there is not until the server's baseline
/// has arrived. Edits that turn up first simply wait in the queue, which is the right thing: they
/// are already in the baseline's own list of what is still on its way.
pub fn apply_due_edits(
    timeline: Res<LocalTimeline>,
    mut ground: ResMut<Ground>,
    mut pending: ResMut<PendingEdits>,
    mut patched: MessageWriter<GroundPatched>,
) {
    if pending.0.is_empty() {
        return;
    }
    let now = timeline.tick();
    let mut waiting = Vec::new();
    for edit in pending.0.drain(..) {
        if Tick(edit.tick) - now > 0 {
            waiting.push(edit);
            continue;
        }
        if let Some(patch) = ground.0.sculpt(&edit.stroke) {
            patched.write(GroundPatched(patch));
        }
    }
    pending.0 = waiting;
}

/// PreUpdate: takes whoever is standing on ground that has just risen up with it.
///
/// Without this, raising the ground under a player buries them: the ground probe reaches
/// `GROUND_SNAP_DIST` below the feet and the new surface is *above* them, so they read as airborne
/// inside solid ground, and the sweep pushes them out through the bottom of it. Measured before
/// this existed — one stroke of a one-metre brush under a standing player, and they fell for ever.
///
/// Only upward. Ground that has been *lowered* leaves whoever was on it in the air, and falling is
/// the right answer to that; putting them down would be a teleport into a hole somebody has just
/// dug, which is a thing you dig holes for.
///
/// It runs on the same tick on both sides — the tick the edit lands on, which is chosen to be
/// outside any rollback window — so the server and the client that predicts this player agree
/// about where the lift put them without either of them replaying it.
pub fn lift_with_the_ground<F: QueryFilter + 'static>(
    ground: Res<Ground>,
    mut patched: MessageReader<GroundPatched>,
    mut players: Query<&mut PlayerState, F>,
) {
    for GroundPatched(patch) in patched.read() {
        let (low, high) = patch.bounds(&ground.0.grid);
        for mut state in players.iter_mut() {
            let at = state.position;
            if at.x < low.x || at.x > high.x || at.z < low.y || at.z > high.y {
                continue;
            }
            let surface = ground.0.height_over(at.x, at.z);
            if at.y < surface {
                state.position.y = surface + SKIN;
                // Whatever downward speed they had belonged to standing on the old ground.
                state.velocity.y = state.velocity.y.max(0.0);
            }
        }
    }
}