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

use crate::physics::{level_geometry, level_geometry_facing};

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

/// A slope, so that there is something to drive up and roll back down.
///
/// Half-extents of the slab, before it is tilted. Wide enough for a vehicle and its mistakes.
pub const RAMP_HALF_EXTENTS: Vec3 = Vec3::new(3.0, 0.5, 6.0);
/// How steep it is, in radians — about 12°, which a vehicle climbs and a walking player does not
/// slide back down.
pub const RAMP_ANGLE: f32 = 0.21;
/// Where the middle of it stands, on the ground plane. Clear of the crates.
pub const RAMP_CENTRE: Vec2 = Vec2::new(14.0, -20.0);

/// Where the ramp sits and how it is turned.
///
/// Derived rather than written down, because the number that matters is not the slab's centre but
/// where its *driving surface* meets the ground: a ramp whose near edge is nine centimetres up is a
/// step, and a vehicle hits it rather than climbing it. Solving for that leaves the slab's lower
/// half buried, which is what a real ramp does too.
pub fn ramp_pose() -> (Vec3, Quat) {
    let (sin, cos) = RAMP_ANGLE.sin_cos();
    // The near top corner, in slab space, is (·, +y, +z); tilting takes it to
    // `y·cos − z·sin` below the centre, so the centre must stand exactly that high.
    let height = RAMP_HALF_EXTENTS.z * sin - RAMP_HALF_EXTENTS.y * cos;
    (
        Vec3::new(RAMP_CENTRE.x, height, RAMP_CENTRE.y),
        // Turning about +X lifts the −Z end, and −Z is forward everywhere in this game, so the ramp
        // climbs away from where players start.
        Quat::from_rotation_x(RAMP_ANGLE),
    )
}

/// Where the vehicle stands at the start of a round, on the ground plane.
///
/// In line with the ramp and a little way in front of it, so that driving straight forward from a
/// standing start arrives at the slope.
pub const VEHICLE_START: Vec2 = Vec2::new(14.0, -8.0);

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
    let (at, facing) = ramp_pose();
    commands.spawn(level_geometry_facing(
        Collider::cuboid(
            RAMP_HALF_EXTENTS.x * 2.0,
            RAMP_HALF_EXTENTS.y * 2.0,
            RAMP_HALF_EXTENTS.z * 2.0,
        ),
        at,
        facing,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of deriving the pose: a vehicle must be able to drive on to the ramp, not
    /// into it. The near edge of the driving surface has to be level with the ground.
    #[test]
    fn the_ramp_meets_the_ground_at_its_near_edge() {
        let (at, facing) = ramp_pose();
        let near_top = at + facing * Vec3::new(0.0, RAMP_HALF_EXTENTS.y, RAMP_HALF_EXTENTS.z);
        assert!(near_top.y.abs() < 1e-5, "the ramp starts {:.3} m off the ground", near_top.y);
    }

    /// And it has to actually go somewhere, or it is a plate on the floor.
    #[test]
    fn the_ramp_climbs() {
        let (at, facing) = ramp_pose();
        let far_top = at + facing * Vec3::new(0.0, RAMP_HALF_EXTENTS.y, -RAMP_HALF_EXTENTS.z);
        assert!(far_top.y > 2.0, "the ramp only reaches {:.2} m", far_top.y);
    }
}
