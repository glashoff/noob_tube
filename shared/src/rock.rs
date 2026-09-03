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

/// Samples between the blocks — one every three metres of grid.
const STRIDE: u32 = 3;

/// How steep a place has to be before rock stands there.
///
/// Below the crossing rock is *painted* at, because the foot of a face is exactly where broken
/// stone ends up and the paint should not be what decides that.
const NEEDS: f32 = 0.18;

/// How far a block reaches, as a multiple of the distance to the next one.
///
/// **This is the whole difference between a wall and a wall with pebbles on it.** A block that does
/// not reach its neighbour can only be an ornament, sitting on a surface that is still visibly the
/// smooth one underneath — which is exactly what the first version of this looked like.
///
/// The number to clear is not one. A corner sits at `0.57735` of `size` along each axis, so a block
/// is `1.155 * size` across, and two neighbours a step apart touch at `REACH = 0.866`. The half
/// step of jitter that keeps them off the lattice can pull a pair `1.5` steps apart, and the size
/// each one drew can be as little as four fifths — so the worst pair on a face still parts, and at
/// `1.3` the ordinary pair overlaps by a third of itself.
///
/// The worst pair parting is not a defect. A crevice is what a rock face has; what it must not have
/// is a window onto the smooth slope underneath, and
/// `a_face_is_covered_rather_than_decorated` is what actually settles that — measured over the
/// built-in map rather than argued from these numbers.
const REACH: f32 = 1.3;

/// The share of that reach a block has at the gentlest ground it appears on.
///
/// Not a probability. Thinning them out at the edge would leave gaps in the middle too — the same
/// coin decides both — so instead every place gets a block and the ones on gentle ground are small.
/// A face is solid, its shoulder is a scatter of stones, and the two are one rule.
const SMALLEST: f32 = 0.34;

/// How far a block is pushed into the slope, as a fraction of its size.
///
/// Deep enough that the row below shows through the gaps between the row above rather than a strip
/// of smooth ground doing it.
const SUNK: f32 = 0.5;

/// How far a corner may be pulled back towards the middle.
///
/// This is what makes a block angular rather than a box. Every corner is pulled in by its own
/// amount, so the faces meet at unequal angles and no two blocks share a profile. At zero they are
/// all the same tidy solid; near one they collapse into splinters.
const RAGGED: f32 = 0.55;

/// The eight directions a block's corners are pushed out along: a cube's, and nothing else.
///
/// **Eight rather than a rounder set, and the reason is the bill.** A face carries thousands of
/// these now that they cover it, and eight corners hull into twelve triangles where fourteen make
/// something nearer thirty. It is also the better shape: a cube pulled about at its corners is a
/// slab with flat sides and hard edges, which is what fractured stone is, while adding the six
/// axes rounds the middle of every face back out again.
///
/// Written out rather than normalised at run time — `0.57735026` is one over root three, and there
/// is no reason to take that square root ten thousand times a map.
const OUT: [Vec3; 8] = [
    Vec3::new(0.57735026, 0.57735026, 0.57735026),
    Vec3::new(-0.57735026, 0.57735026, 0.57735026),
    Vec3::new(0.57735026, -0.57735026, 0.57735026),
    Vec3::new(-0.57735026, -0.57735026, 0.57735026),
    Vec3::new(0.57735026, 0.57735026, -0.57735026),
    Vec3::new(-0.57735026, 0.57735026, -0.57735026),
    Vec3::new(0.57735026, -0.57735026, -0.57735026),
    Vec3::new(-0.57735026, -0.57735026, -0.57735026),
];

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

/// The rock standing at one candidate sample, if one does.
fn rock_at(terrain: &Terrain, ix: u32, iz: u32) -> Option<Rock> {
    let steep = terrain.steepness_at(ix, iz);
    if steep < NEEDS {
        return None;
    }
    let grid = terrain.grid;
    let here = grid.world_of(ix, iz);
    let step = STRIDE as f32 * grid.spacing;
    // Off the lattice point, but by less than half the gap: enough that they do not stand in rows,
    // not so much that two crowd into one place and leave a hole where one of them came from.
    let x = here.x + (hash(ix, iz, 1) - 0.5) * step * 0.5;
    let z = here.y + (hash(ix, iz, 2) - 0.5) * step * 0.5;
    // Full size on a face, a fraction of it where the ground has only just become steep, and a
    // spread of a third either way so that no two neighbours are the same block.
    let grown = SMALLEST + (1.0 - SMALLEST) * steep;
    let size = step * REACH * grown * (0.8 + 0.4 * hash(ix, iz, 3));

    // A quaternion out of four hashes. `try_normalize` rather than `normalize` because four values
    // that all land near zero is not impossible, only unlikely, and the unlikely one would be a
    // NaN quaternion and a rock at no orientation at all.
    let spin = Vec4::new(
        hash(ix, iz, 4) * 2.0 - 1.0,
        hash(ix, iz, 5) * 2.0 - 1.0,
        hash(ix, iz, 6) * 2.0 - 1.0,
        hash(ix, iz, 7) * 2.0 - 1.0,
    );
    let facing = spin.try_normalize().map_or(Quat::IDENTITY, Quat::from_vec4);

    // Squashed along its own axes before it is turned, which is what makes slabs and wedges out of
    // what would otherwise be a family of lumpy cubes.
    let squash = Vec3::new(
        0.62 + hash(ix, iz, 8) * 0.38,
        0.40 + hash(ix, iz, 9) * 0.40,
        0.62 + hash(ix, iz, 10) * 0.38,
    );
    let corners = OUT
        .iter()
        .enumerate()
        .map(|(slot, out)| *out * squash * (size * (1.0 - RAGGED * hash(ix, iz, 16 + slot as u32))))
        .collect();

    // `height_over` is the surface including its relief, which is the ground the rock has to sit on
    // rather than the authored field underneath it.
    let at = Vec3::new(x, terrain.height_over(x, z) - size * SUNK, z);
    Some(Rock { at, facing, corners })
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
            rocks.extend(rock_at(terrain, ix, iz));
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
                    assert!(size.min_element() > 0.05, "a rock {size} across is a sheet of paper");
                    largest = largest.max(size.max_element());
                    count += 1;
                }
            }
        }
        // A corner sits at 0.57735 of `size`, `size` is at most `step * REACH * 1.2`, and the
        // squash only ever shrinks. Anything past that is a block that grew in a way nothing here
        // asked it to.
        let bound = 1.1547 * 3.0 * REACH * 1.2;
        assert!(largest <= bound, "a rock came out {largest:.2} m across, past {bound:.2}");
        println!("{count} rocks, largest {largest:.2} m across");
    }
}
