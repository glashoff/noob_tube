//! Rock, as the height field *describes* rather than as it stores.
//!
//! **A height field cannot make a rock.** It is `y = f(x, z)`: single-valued, so there is no
//! overhang in it, no vertical face, and every cliff is a ramp seen from the side. Bending the
//! samples harder does not fix that and neither does a normal map — the silhouette against the sky
//! stays a straight line, because a straight line is all the data can hold.
//!
//! So above a slope the map stops being the surface and becomes a *brief*: the same steep samples
//! that decide where rock is painted also decide where blocks of stone stand, how big, how turned
//! and how ragged. The height field is the base that says what to build; this builds it.
//!
//! **Reproduced, never sent.** Every number here comes out of a hash of the sample indices, so a
//! server and a client with the same map agree on every rock without exchanging a word about them —
//! and a map that is 1 MB of heights stays 1 MB however many thousand rocks stand on it. Sculpt the
//! ground and the rocks follow, because they were never anything but a function of it.
//!
//! **One hull, two uses.** A rock is a set of corners; their convex hull is the collider, and the
//! drawn faces are read back out of that same hull rather than built beside it. There is no second
//! description to disagree — terrain.md's rule about the shape you see and the shape you walk on,
//! kept by construction rather than by care.
//!
//! Convex on purpose. A hull is the shape parry is fastest and steadiest against, it is what
//! `tools/bake_collider` already reduces the vehicles to, and a heap of convex blocks is what
//! broken stone looks like anyway. What one block cannot be is concave — an arch or a cave has to
//! be several, and at these sizes that is what the heap gives you for free.
//!
//! Unlike [`Terrain::relief_at`](crate::terrain::Terrain::relief_at), this does not avoid `sqrt`.
//! It cannot: a convex hull is geometry, and parry takes square roots to build one. The height
//! field avoids them because a last-ulp disagreement there moves the ground under a player's feet
//! by a whole sample; here the same disagreement moves one corner of one boulder by a nanometre,
//! and both sides are running the same build of the same crate on bit-identical corners.

use avian3d::prelude::*;
use bevy::prelude::*;

use crate::terrain::Terrain;

/// Samples across one slab — the lattice the wall is broken into, three metres of it.
const STRIDE: u32 = 3;

/// How steep a cell has to be before it becomes rock.
const NEEDS: f32 = 0.18;

/// How far a slab is pushed off the surface, in metres, from least to most.
///
/// This is what a wall's roughness *is* here. Neighbouring slabs draw their own numbers, so the one
/// beside you stands at a different height, and the step between them is a face — vertical, because
/// the sides run along the surface normal, and overhanging wherever the ground is past 45°.
const LIFT_LOW: f32 = 0.35;
const LIFT_HIGH: f32 = 1.5;

/// How much of the lift survives on ground that has only just become steep.
///
/// Not zero, and that is a geometric requirement rather than a taste: a slab with no lift is eight
/// points on a plane, which is not a solid, and `convex_hull` answers `None` to it. The rest fades
/// with steepness so the wall grows out of the hillside instead of starting at a line.
const LEAST_LIFT: f32 = 0.35;

/// How far a slab's underside is buried below the surface it replaces, in metres.
///
/// Small, and not for looks. The underside would otherwise sit exactly on the ground mesh that is
/// still drawn beneath it, and two coplanar surfaces at the same depth is z-fighting — a seam that
/// flickers between two shades as the camera moves, on every slab on the map. Burying it also
/// closes the sliver a slab's tilt would otherwise open along its lower edge.
const BURY: f32 = 0.2;

/// How far a slab's top may slide sideways over its own footprint, as a fraction of the step.
///
/// A slab pushed straight out has vertical sides. One whose top has also slid leans, and the edge
/// it leans over is an overhang in the plain sense — something you can stand under, which is the
/// thing a height field can never have and the whole reason this module exists.
const LEAN: f32 = 0.3;

/// One rock: where it stands, how it is turned, and the corners it is made of.
#[derive(Clone, Debug)]
pub struct Rock {
    pub at: Vec3,
    pub facing: Quat,
    /// The corners in the rock's own frame. Their convex hull is the rock — both the collider and
    /// the drawn faces come out of it, so there is no second shape to be wrong.
    pub corners: Vec<Vec3>,
}

impl Rock {
    /// The rock as one convex hull, or nothing if its corners were too nearly flat to make a solid.
    pub fn collider(&self) -> Option<Collider> {
        Collider::convex_hull(self.corners.clone())
    }
}

/// One value in 0..1 from a sample and a slot, exact and the same everywhere.
///
/// The slot is what lets one candidate ask a dozen independent questions — is there a rock, how
/// big, how turned, how ragged in each of fourteen directions — out of one lattice point. Twenty
/// four bits scaled by a power of two, so the division is exact rather than nearly so.
fn hash(ix: u32, iz: u32, slot: u32) -> f32 {
    let mut h = ix
        .wrapping_mul(0x27d4_eb2d)
        ^ iz.wrapping_mul(0x1656_67b1)
        ^ slot.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 13;
    h = h.wrapping_mul(0x297a_2d39);
    h ^= h >> 16;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// The slab one cell of the lattice has become, if that cell is steep enough to be rock.
///
/// **This replaces the surface, it does not stand on it.** The four corners of the cell, taken off
/// the ground itself, are the slab's underside; the same four pushed out along the cell's normal
/// are its top. Neighbouring cells share their corner samples, so the undersides tile the wall
/// exactly and there is no gap and no smooth ground left showing between them — which is the whole
/// difference from scattering blocks onto a face, where the face is still there behind them.
///
/// **The lift is drawn per cell, not per corner sample.** That is the one decision that makes a
/// wall rough rather than merely displaced: two cells that shared a lift would meet flush and the
/// surface would be continuous again, only bumpier. Drawing separately, they meet at a step, and
/// the step is a face — vertical, since the sides run along the normal, and overhanging wherever
/// the ground itself is past 45°.
///
/// The top also slides sideways over its own footprint, which is what turns a straight-sided block
/// into a leaning one. What it leans over is an overhang in the plain sense: something to stand
/// under, and the thing `y = f(x, z)` can never have.
fn slab_at(terrain: &Terrain, ix: u32, iz: u32) -> Option<Rock> {
    let grid = terrain.grid;
    let (jx, jz) = ((ix + STRIDE).min(grid.nx - 1), (iz + STRIDE).min(grid.nz - 1));
    // A cell the rim of the map cut in half is not a cell. Dropping it costs one slab at the very
    // edge and saves a degenerate footprint.
    if jx == ix || jz == iz {
        return None;
    }
    let corner = |x: u32, z: u32| {
        let at = grid.world_of(x, z);
        Vec3::new(at.x, terrain.surface_at(x, z), at.y)
    };
    let surface = [corner(ix, iz), corner(jx, iz), corner(ix, jz), corner(jx, jz)];

    // The steepness of the whole footprint rather than of one of its corners: a slab is as steep as
    // the ground it replaces, and asking a single sample would let a cell straddling the edge of a
    // face come out at either answer depending which corner it was asked about.
    let steep = 0.25
        * (terrain.steepness_at(ix, iz)
            + terrain.steepness_at(jx, iz)
            + terrain.steepness_at(ix, jz)
            + terrain.steepness_at(jx, jz));
    if steep < NEEDS {
        return None;
    }

    // Out of the wall, and the two ways along it. `normalize` is safe here and nowhere near zero:
    // the two edges are a cell apart in x and in z, so their cross product is at least the cell's
    // own area however the ground is tilted.
    let along = surface[1] - surface[0];
    let down = surface[2] - surface[0];
    let mut normal = down.cross(along).normalize();
    if normal.y < 0.0 {
        normal = -normal;
    }
    let sideways = normal.cross(along.normalize());

    // The cell's own index, so that the lift is a property of the slab and not of the samples it
    // shares with the slab beside it.
    let (cx, cz) = (ix / STRIDE, iz / STRIDE);
    let fade = LEAST_LIFT + (1.0 - LEAST_LIFT) * steep;
    let step = STRIDE as f32 * grid.spacing;
    let lean = (hash(cx, cz, 1) - 0.5) * along.normalize() * step * LEAN
        + (hash(cx, cz, 2) - 0.5) * sideways * step * LEAN;
    // Below the surface, so that nothing of this is coplanar with the ground mesh under it.
    let under = surface.map(|at| at - normal * BURY);
    let over = surface.map(|at| {
        let slot = (at.x.to_bits() ^ at.z.to_bits()) & 3;
        let drawn = hash(cx, cz, 8 + slot);
        at + normal * ((LIFT_LOW + (LIFT_HIGH - LIFT_LOW) * drawn) * fade) + lean
    });

    // The middle of the eight, so that the corners are small numbers about their own origin rather
    // than world coordinates a hull would lose precision on out at the rim of a kilometre of map.
    let middle = (under.iter().chain(&over).copied().sum::<Vec3>()) / 8.0;
    let corners = under.iter().chain(&over).map(|at| *at - middle).collect();
    Some(Rock { at: middle, facing: Quat::IDENTITY, corners })
}

/// Every rock standing on one tile.
///
/// **Half-open on the high edge, and that is the whole of the bookkeeping.** Neighbouring tiles
/// share their boundary samples — a tile of `n` cells needs `n + 1` grid points — so a candidate
/// sitting exactly on a boundary would otherwise be built by both tiles and stand there twice, as
/// two meshes in the same place and two colliders fighting over the same volume. Taking the low
/// edge and not the high one gives every candidate to exactly one tile.
pub fn rocks_of_tile(terrain: &Terrain, tx: u32, tz: u32) -> Vec<Rock> {
    let grid = terrain.grid;
    let (ix0, ix1, iz0, iz1) = grid.tile_samples(tx, tz);
    // The candidates are on a lattice of the *map*, not of the tile, so which tile a rock belongs
    // to cannot change with the tiling.
    let first = |i: u32| i.div_ceil(STRIDE) * STRIDE;
    let mut rocks = Vec::new();
    let mut iz = first(iz0);
    while iz < iz1 {
        let mut ix = first(ix0);
        while ix < ix1 {
            rocks.extend(slab_at(terrain, ix, iz));
            ix += STRIDE;
        }
        iz += STRIDE;
    }
    rocks
}

/// Every rock of one tile as a single compound collider, or nothing if the tile has none.
///
/// One body for a tileful rather than one per rock: a map can carry thousands, and thousands of
/// static bodies is thousands of entities, thousands of transforms and thousands of entries in
/// every broad-phase pass — for shapes that never move and are already grouped by the thing that
/// culls and rebuilds them. The compound's parts are in world space and the body sits at the
/// origin, which is what keeps a rock's position the same number here as in the mesh.
pub fn tile_collider(terrain: &Terrain, tx: u32, tz: u32) -> Option<Collider> {
    let parts: Vec<_> = rocks_of_tile(terrain, tx, tz)
        .into_iter()
        .filter_map(|rock| Some((rock.at, rock.facing, rock.collider()?)))
        .collect();
    (!parts.is_empty()).then(|| Collider::compound(parts))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::default_terrain;

    /// The same map gives the same rocks, corner for corner.
    ///
    /// This is the whole claim the design rests on. Nothing about a rock is stored or sent: a
    /// server and a client agree about a boulder only because they both derived it from the same
    /// samples. If that ever stopped being exactly true, a player would be stopped by a rock that
    /// is not drawn where they are standing — and it would happen on one machine and not the other,
    /// which is the hardest kind of report to act on.
    #[test]
    fn the_same_ground_makes_the_same_rocks() {
        let terrain = default_terrain();
        for (tx, tz) in [(0, 0), (3, 2), (5, 5)] {
            let once = rocks_of_tile(&terrain, tx, tz);
            let twice = rocks_of_tile(&terrain, tx, tz);
            assert_eq!(once.len(), twice.len(), "tile {tx},{tz} changed its mind about how many");
            for (a, b) in once.iter().zip(&twice) {
                assert_eq!(a.at, b.at);
                assert_eq!(a.facing, b.facing);
                assert_eq!(a.corners, b.corners);
            }
        }
    }

    /// No two tiles claim the same rock, and none is dropped between them.
    ///
    /// Tiles share their boundary samples, so a candidate on a boundary is one the tiling could
    /// easily build twice — two hulls in one place, drawn twice and colliding with each other.
    /// Counting every tile against one pass over the whole map catches both that and the opposite
    /// mistake, a strip of rock nobody owns.
    #[test]
    fn every_rock_belongs_to_exactly_one_tile() {
        let terrain = default_terrain();
        let (wide, deep) = terrain.grid.tiles();
        let mut places: Vec<[u32; 3]> = Vec::new();
        for tz in 0..deep {
            for tx in 0..wide {
                for rock in rocks_of_tile(&terrain, tx, tz) {
                    places.push([rock.at.x.to_bits(), rock.at.y.to_bits(), rock.at.z.to_bits()]);
                }
            }
        }
        let total = places.len();
        places.sort_unstable();
        places.dedup();
        assert_eq!(total, places.len(), "{} rocks stand in another rock", total - places.len());
        assert!(total > 200, "only {total} rocks on the whole map — the density is not doing much");
    }

    /// Rock stands on steep ground and nowhere else.
    ///
    /// The flat ground round the spawn is the case that matters: `HOME_FLAT` exists so that the
    /// spawns, the ramp and the crates stand on a plane that nothing shapes, and a boulder dropped
    /// into the middle of it would be exactly the sort of thing you find by walking into it.
    #[test]
    fn nothing_stands_on_the_flat() {
        let terrain = default_terrain();
        let (wide, deep) = terrain.grid.tiles();
        for tz in 0..deep {
            for tx in 0..wide {
                for rock in rocks_of_tile(&terrain, tx, tz) {
                    let flat = rock.at.x * rock.at.x + rock.at.z * rock.at.z < 40.0 * 40.0;
                    assert!(!flat, "a rock at {} is inside the flat the level stands on", rock.at);
                }
            }
        }
    }

    /// A face comes out covered, not sprinkled.
    ///
    /// This is the claim the whole shape of the thing rests on, and the one the first version got
    /// wrong: blocks smaller than the gap between them are ornaments on a surface that is still
    /// visibly the smooth one underneath. Measured rather than argued — every steep place on the
    /// map, counted against the footprints of the blocks standing near it.
    ///
    /// A footprint is the hull's bounding box, which overstates a rotated block a little. That is
    /// why the bar is not one: what it is really watching for is the number falling back towards
    /// the half it was at when a coin decided which places got a block at all.
    #[test]
    fn a_face_is_covered_rather_than_decorated() {
        let terrain = default_terrain();
        let grid = terrain.grid;
        let (wide, deep) = terrain.grid.tiles();
        let mut prints: Vec<(Vec3, Vec3)> = Vec::new();
        for tz in 0..deep {
            for tx in 0..wide {
                for rock in rocks_of_tile(&terrain, tx, tz) {
                    let hull = rock.collider().expect("a solid");
                    let box_of = hull.aabb(rock.at, rock.facing);
                    prints.push((box_of.min, box_of.max));
                }
            }
        }
        let (mut steep, mut under) = (0usize, 0usize);
        for iz in (0..grid.nz).step_by(STRIDE as usize) {
            for ix in (0..grid.nx).step_by(STRIDE as usize) {
                // The ground a player would call a face, rather than the shoulder either side of
                // it: what is meant to be solid is the part that is fully rock.
                if terrain.steepness_at(ix, iz) < 0.95 {
                    continue;
                }
                steep += 1;
                let here = grid.world_of(ix, iz);
                if prints.iter().any(|(low, high)| {
                    here.x >= low.x && here.x <= high.x && here.y >= low.z && here.y <= high.z
                }) {
                    under += 1;
                }
            }
        }
        let covered = under as f32 / steep as f32;
        println!("{under} of {steep} places on a face are under a block ({:.0}%)", covered * 100.0);
        assert!(covered > 0.9, "only {:.0}% of a face is covered", covered * 100.0);
    }

    /// Every rock closes into a solid, and the solid is the size it was asked to be.
    ///
    /// A hull of fourteen points can fail — pull enough corners in and they fall onto a plane, and
    /// `convex_hull` answers `None` rather than a flat rock. That would be a rock drawn and not
    /// collided with, or the reverse, so `RAGGED` is only allowed to be as large as this passes at.
    #[test]
    fn every_rock_is_a_solid() {
        let terrain = default_terrain();
        let (wide, deep) = terrain.grid.tiles();
        let (mut count, mut largest) = (0usize, 0.0f32);
        for tz in 0..deep {
            for tx in 0..wide {
                for rock in rocks_of_tile(&terrain, tx, tz) {
                    let hull = rock.collider().expect("a rock that is a solid");
                    let box_of = hull.aabb(Vec3::ZERO, Quat::IDENTITY);
                    let size = box_of.max - box_of.min;
                    assert!(size.min_element() > 0.05, "a slab {size} across is a sheet of paper");
                    // Sideways only. A slab is as tall as the ground it replaces, and on a steep
                    // face a three-metre cell drops five metres — that is the terrain's number and
                    // not this module's, so bounding it here would be measuring the map.
                    largest = largest.max(size.x.max(size.z));
                    count += 1;
                }
            }
        }
        // One cell across, plus the lean its top may slide either way, plus the lift — which on a
        // wall points sideways, because the normal of a wall is horizontal.
        let step = STRIDE as f32;
        let bound = step * (1.0 + LEAN) + LIFT_HIGH;
        assert!(largest <= bound, "a slab came out {largest:.2} m wide, past {bound:.2}");
        println!("{count} slabs, widest {largest:.2} m");
    }
}
