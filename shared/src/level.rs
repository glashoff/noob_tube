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

use bevy::math::Vec3;

use crate::collision::CollisionWorld;

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
pub fn spawn_point(index: usize) -> Vec3 {
    Vec3::new(index as f32 * 2.0, 0.0, 0.0)
}

/// Builds the collision geometry. Identical on both sides, by construction.
pub fn collision_world() -> CollisionWorld {
    let mut world = CollisionWorld::new();
    world.add_trimesh(
        vec![
            Vec3::new(-HALF_EXTENT, 0.0, -HALF_EXTENT),
            Vec3::new(HALF_EXTENT, 0.0, -HALF_EXTENT),
            Vec3::new(HALF_EXTENT, 0.0, HALF_EXTENT),
            Vec3::new(-HALF_EXTENT, 0.0, HALF_EXTENT),
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    for centre in CRATES {
        world.add_cuboid(centre, Vec3::splat(CRATE_HALF_EXTENT));
    }
    world.rebuild();
    world
}
