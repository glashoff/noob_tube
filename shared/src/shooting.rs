//! Hitscan: the shot resolves the instant it is fired, against the capsule that moves.
//!
//! The hitbox is the movement capsule and nothing else — the same shape the player collides with,
//! so what stops you walking is what stops a bullet. Per-bone hitboxes arrive with the real models
//! in M2; until then a head shot and a shin shot are the same shot.
//!
//! **This is not lag compensated.** The server tests against where a target is *now*, not where the
//! shooter saw them, so at any real ping you must lead a moving target. Fixing it needs a position
//! history on the server to rewind into, plus the shooter's interpolation delay to know how far
//! back — lightyear has `InputConfig::lag_compensation` for the second half.

use bevy::prelude::*;
use rapier3d::parry::query::RayCast;
use rapier3d::prelude::*;
use serde::{Deserialize, Serialize};

use crate::collision::CollisionWorld;
use crate::movement::{
    CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET, CROUCH_CAPSULE_HALF_HEIGHT,
    CROUCH_CAPSULE_Y_OFFSET,
};

/// How far a shot reaches. The arena is 500 m across, so this crosses most of it.
pub const WEAPON_RANGE: f32 = 200.0;
/// Damage per hit. Three shots to kill from full health.
pub const WEAPON_DAMAGE: u8 = 34;
/// Ticks between shots. Eight at 64 Hz is 125 ms, or 480 rounds per minute.
pub const FIRE_INTERVAL_TICKS: u8 = 8;
/// Health a player spawns with.
pub const MAX_HEALTH: u8 = 100;

/// How much of a player is left.
///
/// Replicated but **not** predicted: the client has no business guessing whether its shot landed.
/// A predicted kill that the server disagreed with would have to be taken back, and there is no
/// graceful way to un-kill someone on screen.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Health(pub u8);

impl Default for Health {
    fn default() -> Self {
        Self(MAX_HEALTH)
    }
}

impl Health {
    pub fn is_alive(&self) -> bool {
        self.0 > 0
    }

    /// Applies damage, saturating at zero. Returns true if this was the killing blow.
    pub fn hurt(&mut self, amount: u8) -> bool {
        let was_alive = self.is_alive();
        self.0 = self.0.saturating_sub(amount);
        was_alive && !self.is_alive()
    }
}

/// Where a shot starts and which way it goes.
///
/// From the eye, not the capsule centre, because that is what the shooter aimed with. Yaw and pitch
/// are the same angles the input carried, so the server reconstructs exactly the direction the
/// client was looking down.
pub fn aim_ray(eye: Vec3, yaw: f32, pitch: f32) -> (Vec3, Vec3) {
    // Matches the camera's `EulerRot::YXZ`: yaw about Y, then pitch about the turned X. Forward is
    // -Z at rest, the convention movement uses.
    let direction = Quat::from_euler(EulerRot::YXZ, yaw, pitch, 0.0) * Vec3::NEG_Z;
    (eye, direction.normalize_or_zero())
}

/// Distance along the ray at which it enters a player's capsule, if it does.
///
/// `feet` is the player's position, the same one movement works in.
pub fn hit_distance(
    origin: Vec3,
    direction: Vec3,
    feet: Vec3,
    crouching: bool,
    max_distance: f32,
) -> Option<f32> {
    let (half_height, y_offset) = if crouching {
        (CROUCH_CAPSULE_HALF_HEIGHT, CROUCH_CAPSULE_Y_OFFSET)
    } else {
        (CAPSULE_HALF_HEIGHT, CAPSULE_Y_OFFSET)
    };
    let capsule = Capsule::new_y(half_height, CAPSULE_RADIUS);
    let pose = Pose::from_translation(feet + Vec3::Y * y_offset);
    let ray = Ray::new(origin.into(), direction.into());
    // `solid` so a shot fired from inside a capsule counts as an immediate hit rather than passing
    // through and striking the far wall of it.
    capsule.cast_ray(&pose, &ray, max_distance, true)
}

/// Who a shot hits, out of the targets offered.
///
/// The level is tested too and wins ties by being nearer: a target behind a crate is behind cover,
/// not merely obscured. `targets` supplies each candidate's entity, feet and stance; the shooter
/// must not be among them, or they shoot themselves at zero distance.
pub fn resolve<T: IntoIterator<Item = (Entity, Vec3, bool)>>(
    world: &CollisionWorld,
    origin: Vec3,
    direction: Vec3,
    targets: T,
) -> Option<(Entity, f32)> {
    // Anything past the wall is not a target, so the wall sets the budget for the whole search.
    let reach = world
        .raycast(origin, direction, WEAPON_RANGE)
        .unwrap_or(WEAPON_RANGE);

    let mut best: Option<(Entity, f32)> = None;
    for (entity, feet, crouching) in targets {
        let Some(distance) = hit_distance(origin, direction, feet, crouching, reach) else {
            continue;
        };
        if best.is_none_or(|(_, nearest)| distance < nearest) {
            best = Some((entity, distance));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world with a floor and one crate, matching the level's own.
    fn world_with_cover() -> CollisionWorld {
        let mut world = CollisionWorld::new();
        world.add_trimesh(
            vec![
                Vec3::new(-100.0, 0.0, -100.0),
                Vec3::new(100.0, 0.0, -100.0),
                Vec3::new(100.0, 0.0, 100.0),
                Vec3::new(-100.0, 0.0, 100.0),
            ],
            vec![[0, 1, 2], [0, 2, 3]],
        );
        world.add_cuboid(Vec3::new(0.0, 1.0, -5.0), Vec3::splat(1.0));
        world.rebuild();
        world
    }

    fn empty_world() -> CollisionWorld {
        let mut world = CollisionWorld::new();
        world.rebuild();
        world
    }

    const TARGET: Entity = Entity::from_raw_u32(1).unwrap();

    /// Yaw 0 looks down -Z, the same convention movement uses. Getting this wrong would make every
    /// shot miss by exactly the angle nobody thinks to check.
    #[test]
    fn a_level_gaze_points_forward() {
        let (_, direction) = aim_ray(Vec3::ZERO, 0.0, 0.0);
        assert!((direction - Vec3::NEG_Z).length() < 1e-5, "{direction:?}");
    }

    #[test]
    fn looking_up_raises_the_shot() {
        let (_, direction) = aim_ray(Vec3::ZERO, 0.0, 0.5);
        assert!(direction.y > 0.0, "{direction:?}");
    }

    #[test]
    fn a_shot_down_the_middle_hits() {
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.0);
        let target = Vec3::new(0.0, 0.0, -10.0);
        let hit = resolve(&empty_world(), origin, direction, [(TARGET, target, false)]);
        let (entity, distance) = hit.expect("a target straight ahead was not hit");
        assert_eq!(entity, TARGET);

        // Not a full radius short of the centre: a level shot from eye height passes above the
        // capsule's widest point and enters the rounded cap, where it is narrower. The cap's
        // centre is at CAPSULE_Y_OFFSET + CAPSULE_HALF_HEIGHT = 1.35 m, the eye at 1.59 m.
        let above_cap = 1.59 - (CAPSULE_Y_OFFSET + CAPSULE_HALF_HEIGHT);
        let half_width = (CAPSULE_RADIUS * CAPSULE_RADIUS - above_cap * above_cap).sqrt();
        assert!(
            (distance - (10.0 - half_width)).abs() < 0.02,
            "entered at {distance}, expected about {}",
            10.0 - half_width
        );
    }

    #[test]
    fn a_shot_beside_the_target_misses() {
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.0);
        // A metre to the side, well clear of the 0.35 m radius.
        let target = Vec3::new(1.0, 0.0, -10.0);
        assert!(resolve(&empty_world(), origin, direction, [(TARGET, target, false)]).is_none());
    }

    /// Cover has to work, or the level is decoration.
    #[test]
    fn a_crate_stops_the_shot() {
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.0);
        // The crate sits at z = -5; the target is behind it.
        let target = Vec3::new(0.0, 0.0, -10.0);
        assert!(
            resolve(&world_with_cover(), origin, direction, [(TARGET, target, false)]).is_none(),
            "shot through a crate"
        );
    }

    /// A crouched player is a smaller target, which is the point of crouching.
    #[test]
    fn crouching_ducks_under_a_level_shot() {
        // Aimed at standing eye height, ten metres out.
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.0);
        let target = Vec3::new(0.0, 0.0, -10.0);
        assert!(hit_distance(origin, direction, target, false, WEAPON_RANGE).is_some());
        assert!(
            hit_distance(origin, direction, target, true, WEAPON_RANGE).is_none(),
            "a crouched player was hit by a shot at standing head height"
        );
    }

    #[test]
    fn the_nearest_of_two_targets_is_hit() {
        let near = Entity::from_raw_u32(1).unwrap();
        let far = Entity::from_raw_u32(2).unwrap();
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.0);
        let hit = resolve(
            &empty_world(),
            origin,
            direction,
            [(far, Vec3::new(0.0, 0.0, -20.0), false), (near, Vec3::new(0.0, 0.0, -5.0), false)],
        );
        assert_eq!(hit.map(|(entity, _)| entity), Some(near));
    }

    #[test]
    fn three_shots_kill() {
        let mut health = Health::default();
        assert!(!health.hurt(WEAPON_DAMAGE));
        assert!(!health.hurt(WEAPON_DAMAGE));
        assert!(health.hurt(WEAPON_DAMAGE), "the third shot did not kill");
        assert!(!health.is_alive());
    }

    /// Damage to a corpse must not report a second kill, or a body takes several deaths.
    #[test]
    fn only_one_shot_gets_the_kill() {
        let mut health = Health::default();
        health.hurt(MAX_HEALTH);
        assert!(!health.hurt(WEAPON_DAMAGE));
    }
}
