//! The level's geometry, as both sides must see it.
//!
//! Client and server have to collide against exactly the same shapes. If they do not, the client's
//! prediction and the server's authority disagree about where a player can stand, and every step
//! near the difference produces a correction the player sees as a stutter. Keeping the numbers in
//! one place is the cheapest way to make that impossible.
//!
//! The visible meshes stay in the client, built from these same constants. That split is
//! deliberate — real levels use simplified collision geometry, and the two will diverge in shape
//! long before they diverge in intent.

use avian3d::prelude::Collider;
use bevy::prelude::*;

use crate::physics::level_geometry;

/// Half the side length of the ground plane, in metres.
pub const HALF_EXTENT: f32 = 250.0;

/// Half-extents of one crate. They are cubes, 2 m on a side.
pub const CRATE_HALF_EXTENT: f32 = 1.0;

/// Where the crates stand, at their centres. Something to bump into so collision and sliding are
/// visible at all.
pub const CRATES: [Vec3; 3] = [
    Vec3::new(6.0, 1.0, -8.0),
    Vec3::new(-5.0, 1.0, -12.0),
    Vec3::new(0.0, 1.0, -18.0),
];

/// Where the nth player starts.
///
/// Nothing clever: players stand two metres apart along X, which is enough to tell capsules apart
/// while there are a handful of them. Real spawn points come with real levels.
///
/// The index the server passes is a count of existing players, so it climbs as people reconnect —
/// a client that joins after two others have left starts at x = 6 rather than x = 0. Harmless on an
/// empty plane, wrong on a real map, and fixed by picking a free spawn point rather than counting.
pub fn spawn_point(index: usize) -> Vec3 {
    Vec3::new(index as f32 * 2.0, 0.0, 0.0)
}

/// Startup: builds the collision geometry. Identical on both sides, by construction.
///
/// One entity per shape, each a static rigid body on the level layer. The ground is a triangle mesh
/// rather than a box because that is what a real level's collision geometry is, and using the shape
/// we will actually ship keeps the awkward cases — a shape cast against a triangle mesh is accurate
/// only to a few millimetres — in front of us rather than behind a placeholder.
pub fn spawn_level(mut commands: Commands) {
    commands.spawn(level_geometry(
        Collider::trimesh(
            vec![
                Vec3::new(-HALF_EXTENT, 0.0, -HALF_EXTENT),
                Vec3::new(HALF_EXTENT, 0.0, -HALF_EXTENT),
                Vec3::new(HALF_EXTENT, 0.0, HALF_EXTENT),
                Vec3::new(-HALF_EXTENT, 0.0, HALF_EXTENT),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        ),
        Vec3::ZERO,
    ));
    for centre in CRATES {
        // Avian sizes a cuboid by its full side lengths, where rapier takes half-extents.
        commands.spawn(level_geometry(
            Collider::cuboid(
                CRATE_HALF_EXTENT * 2.0,
                CRATE_HALF_EXTENT * 2.0,
                CRATE_HALF_EXTENT * 2.0,
            ),
            centre,
        ));
    }
}
