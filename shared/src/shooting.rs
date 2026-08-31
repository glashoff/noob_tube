//! Hitscan: the shot resolves the instant it is fired, against the capsule that moves.
//!
//! The hitbox is the movement capsule and nothing else — the same shape the player collides with,
//! so what stops you walking is what stops a bullet. Per-bone hitboxes arrive with the real models
//! in M2; until then a head shot and a shin shot are the same shot.
//!
//! Nothing here knows what time it is. The caller passes in the positions to test against, and on
//! the server those come from [`crate::lag_compensation`] — the moment the shooter's screen was
//! showing, not the present one. Keeping that out of here is what lets every rule below be checked
//! by a test that needs no network at all.

use bevy::prelude::*;
use rapier3d::parry::query::RayCast;
use rapier3d::prelude::*;
use serde::{Deserialize, Serialize};

use crate::collision::CollisionWorld;
use crate::player::{PlayerInput, PlayerState};
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

/// Where a shot stopped, and in whom.
///
/// Always a result, never a miss: a shot that hits nobody still ends somewhere — on a wall, or at
/// the weapon's range — and that endpoint is what every client needs in order to draw the thing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    /// How far along the ray it stopped.
    pub distance: f32,
    /// The player it stopped in, if it stopped in one.
    pub target: Option<Entity>,
}

impl Shot {
    /// The point it stopped at.
    pub fn point(&self, origin: Vec3, direction: Vec3) -> Vec3 {
        origin + direction * self.distance
    }
}

/// Where a shot stops, given the targets offered.
///
/// The level is tested too and wins ties by being nearer: a target behind a crate is behind cover,
/// not merely obscured. `targets` supplies each candidate's entity, feet and stance; the shooter
/// must not be among them, or they shoot themselves at zero distance.
pub fn resolve<T: IntoIterator<Item = (Entity, Vec3, bool)>>(
    world: &CollisionWorld,
    origin: Vec3,
    direction: Vec3,
    targets: T,
) -> Shot {
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
    match best {
        Some((entity, distance)) => Shot { distance, target: Some(entity) },
        None => Shot { distance: reach, target: None },
    }
}

/// A shot that was actually taken: where it started, which way it went, and where it stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fired {
    pub origin: Vec3,
    pub direction: Vec3,
    pub shot: Shot,
}

impl Fired {
    /// Where it stopped.
    pub fn point(&self) -> Vec3 {
        self.shot.point(self.origin, self.direction)
    }
}

/// Turns a held trigger into a shot, or into nothing.
///
/// The one place that decision is made, and it is made identically on both sides — the server to
/// score the shot, the shooter's own client to draw it without waiting to be told. That is the same
/// reason [`step_players`](crate::simulation::step_players) is one system rather than one per
/// binary: two copies of a rule is precisely how a prediction stops matching, and the drift shows up
/// as a shot landing somewhere the player did not aim rather than as a compile error.
///
/// Call it **before** [`PlayerState::apply_input`], which starts the cooldown and so makes the
/// answer no. The angles come from `input`, not from the replicated [`Aim`](crate::player::Aim):
/// `Aim` is written at the end of a tick, so reading it here would aim every shot with the previous
/// tick's angles.
pub fn fire<T: IntoIterator<Item = (Entity, Vec3, bool)>>(
    world: &CollisionWorld,
    state: &PlayerState,
    input: &PlayerInput,
    targets: T,
) -> Option<Fired> {
    if !state.is_firing(input) {
        return None;
    }
    let (origin, direction) = aim_ray(state.eye_position(), input.yaw, input.pitch);
    Some(Fired {
        origin,
        direction,
        shot: resolve(world, origin, direction, targets),
    })
}

/// What every client is told about a shot, so that a shot can be seen rather than only felt.
///
/// Sent unreliably and to everyone, including the shooter. Unreliably because a tracer is over in
/// a twentieth of a second: a retransmitted one would arrive after the moment it belongs to, and
/// drawing it then would be worse than not drawing it at all.
///
/// Deliberately small. There is no surface normal in here, because every client already holds the
/// same [`CollisionWorld`] built from the same numbers and can cast the ray itself to find one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShotFired {
    /// Which peer fired, so a client can tell its own shots from everyone else's.
    pub shooter: u64,
    /// The muzzle — the shooter's eye, which is where the ray was cast from.
    pub from: Vec3,
    /// Where it stopped: a player, a wall, or the end of its range.
    pub to: Vec3,
    /// Whether it stopped in a player. Decides blood against a bullet hole, and whether the
    /// shooter gets a hit marker.
    pub hit_player: bool,
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
        assert_eq!(hit.target, Some(TARGET), "a target straight ahead was not hit");
        let distance = hit.distance;

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
        assert!(resolve(&empty_world(), origin, direction, [(TARGET, target, false)]).target.is_none());
    }

    /// Cover has to work, or the level is decoration.
    #[test]
    fn a_crate_stops_the_shot() {
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.0);
        // The crate sits at z = -5; the target is behind it.
        let target = Vec3::new(0.0, 0.0, -10.0);
        let shot = resolve(&world_with_cover(), origin, direction, [(TARGET, target, false)]);
        assert!(shot.target.is_none(), "shot through a crate");
        // And it stopped at the crate's near face, which is what the bullet hole is drawn on.
        assert!((shot.distance - 4.0).abs() < 0.01, "stopped at {}", shot.distance);
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
        assert_eq!(hit.target, Some(near));
    }

    /// A shot that hits nothing still ends somewhere, or there is no tracer to draw.
    #[test]
    fn a_shot_into_the_open_ends_at_its_range() {
        let (origin, direction) = aim_ray(Vec3::new(0.0, 1.59, 0.0), 0.0, 0.5);
        let shot = resolve(&empty_world(), origin, direction, []);
        assert_eq!(shot.target, None);
        assert_eq!(shot.distance, WEAPON_RANGE);
        let end = shot.point(origin, direction);
        assert!(end.y > origin.y, "an upward shot ended below where it started");
    }

    /// The whole of the trigger rule, in the one place both sides call.
    #[test]
    fn firing_needs_a_trigger_and_a_ready_weapon() {
        let world = empty_world();
        let state = PlayerState::default();
        let idle = PlayerInput::default();
        let held = PlayerInput { fire: true, ..PlayerInput::default() };

        assert!(fire(&world, &state, &idle, []).is_none(), "fired without a trigger");
        assert!(fire(&world, &state, &held, []).is_some(), "a ready weapon did not fire");

        let cooling = PlayerState { fire_cooldown: 1, ..PlayerState::default() };
        assert!(fire(&world, &cooling, &held, []).is_none(), "fired while cooling down");
    }

    /// The angles come from the input, not from anywhere the client could not reproduce. Getting
    /// this wrong aims every shot one tick stale, which is invisible until a client predicts it.
    #[test]
    fn a_shot_goes_where_the_input_points() {
        let held = PlayerInput { fire: true, yaw: 0.0, pitch: 0.0, ..PlayerInput::default() };
        let fired = fire(&empty_world(), &PlayerState::default(), &held, []).expect("fired");
        assert!((fired.direction - Vec3::NEG_Z).length() < 1e-5, "{:?}", fired.direction);

        let turned = PlayerInput { yaw: core::f32::consts::FRAC_PI_2, ..held };
        let fired = fire(&empty_world(), &PlayerState::default(), &turned, []).expect("fired");
        assert!((fired.direction - Vec3::NEG_X).length() < 1e-5, "{:?}", fired.direction);
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
