//! Things in the world that move but are nobody's player.
//!
//! There is one kind so far: a crate that rides up and down. It exists to be shot at while moving,
//! which is the case lag compensation has to cover for anything that is not a player — a lift, a
//! swinging door, a train.
//!
//! There are two kinds now, and the difference is one variant of [`RigidBody`]. A **bobbing** crate
//! is kinematic: it goes where the server puts it and nothing pushes back, which is what an
//! animation is. A **loose** crate is dynamic: it falls, it stacks, and a shot moves it. The second
//! is the first thing in this game the physics solver actually does work for, and the reason it is
//! affordable at all is that `lightyear_avian3d` can roll a solver back.
//!
//! **The animation runs on the server alone.** A crate is a kinematic Avian body whose [`Position`]
//! the server writes each tick; clients receive that position like any other replicated component
//! and interpolate it. Nothing on a client works out where the crate ought to be. That is deliberate
//! even though the motion is a pure function of the tick and every client *could* compute it: the
//! moment anything can stop, push or break the crate, a locally computed one is wrong, and the
//! version that is wrong later is not worth being right now.
//!
//! What it is *not* is terrain. It stops bullets, because a shot tests every hitbox and takes the
//! nearest, but it does not stop feet: it sits on [`Layer::Body`](crate::physics::Layer::Body),
//! which the level queries do not see. Standing on a lift is a separate piece of work.
//!
//! ### Why kinematic rather than dynamic
//!
//! A kinematic body moves where it is put and nothing pushes it back. That is what an animation is.
//! A dynamic crate — one that falls, and that a player could shove — is the next thing this becomes,
//! and the change is one variant of [`RigidBody`]; everything around it, replication included,
//! already works the way it would need to.

use avian3d::prelude::*;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

/// A box-shaped prop, as everyone sees it.
///
/// Only the size is in here, because only the size is constant: where it *is* travels as
/// [`Position`], which lightyear replicates and interpolates for every Avian body. Sent once per
/// entity rather than per update — re-sending half-extents sixty times a second for a value that
/// never changes is exactly the sort of thing the old design did by putting both in one component.
#[derive(Component, Clone, Copy, Debug, PartialEq, Reflect, Serialize, Deserialize)]
#[reflect(Component)]
pub struct Prop {
    /// Half the size of the box.
    pub half_extents: Vec3,
}

impl Prop {
    /// The collider that shape corresponds to.
    ///
    /// Avian sizes a cuboid by its full side lengths, where the hitbox is written in half-extents.
    pub fn collider(&self) -> Collider {
        Collider::cuboid(
            self.half_extents.x * 2.0,
            self.half_extents.y * 2.0,
            self.half_extents.z * 2.0,
        )
    }
}

/// A prop that slides back and forth along one axis, forever.
///
/// Server-only, and never replicated: what reaches a client is the [`Position`] this produces.
#[derive(Component, Clone, Copy, Debug, Reflect)]
#[reflect(Component)]
pub struct Bobbing {
    /// The middle of the travel.
    pub centre: Vec3,
    /// Which way it travels, and how far to each side.
    pub reach: Vec3,
    /// Seconds for one full there-and-back.
    pub period: f32,
    /// Half the size of the box.
    pub half_extents: Vec3,
}

impl Bobbing {
    /// Where the prop is at a moment.
    ///
    /// A pure function of the time, not an accumulation, so the crate cannot drift: a server that
    /// paused for a second comes back where it should be rather than a second behind. That is also
    /// what lets its history agree with the rule that made it.
    pub fn position_at(&self, seconds: f32) -> Vec3 {
        let phase = core::f32::consts::TAU * seconds / self.period.max(f32::MIN_POSITIVE);
        self.centre + self.reach * phase.sin()
    }

    /// The shape it presents, which never changes.
    pub fn prop(&self) -> Prop {
        Prop { half_extents: self.half_extents }
    }
}

/// Where the loose crates start, and how big they are.
///
/// Stacked with a small offset rather than squarely on top of each other, so they settle into a
/// leaning pile instead of a column that could plausibly be one box: a screenshot has to show that
/// the solver did something. They are dropped from a little above their resting height for the same
/// reason — the first second of the round shows them fall.
pub const LOOSE_HALF_EXTENT: f32 = 0.5;
pub const LOOSE_CRATES: [Vec3; 4] = [
    Vec3::new(-8.0, 0.6, -6.0),
    Vec3::new(-8.0, 1.7, -6.0),
    Vec3::new(-7.85, 2.8, -6.15),
    Vec3::new(-8.1, 3.9, -5.9),
];

/// The shape every loose crate has. Its weight is [`LOOSE_DENSITY`], which only the server needs.
pub fn loose_prop() -> Prop {
    Prop { half_extents: Vec3::splat(LOOSE_HALF_EXTENT) }
}

/// How heavy a loose crate is, in kilograms per cubic metre.
///
/// Avian derives mass from the collider's volume and this, and its default of 1 makes a cubic-metre
/// box weigh a kilogram — a shot then launches it at forty metres a second. Softwood packed loosely
/// is around this, so a crate of a cubic metre comes out at forty kilograms and a hit shoves it
/// rather than firing it across the map.
pub const LOOSE_DENSITY: f32 = 40.0;

/// The moving crates the level starts with.
///
/// They ride high enough that their lowest point clears a standing player, because a crate you can
/// walk through is more confusing than one out of reach — see the note above about terrain.
///
/// The period sets how much lag compensation there is to see: at three seconds and 1.5 m of reach
/// the crate passes its middle at about 3.1 m/s, so a shooter at 300 ms of ping is aiming about a
/// metre from where the server would otherwise test — most of the crate's own height.
pub const MOVING_CRATES: [Bobbing; 2] = [
    Bobbing {
        centre: Vec3::new(-3.0, 4.0, -10.0),
        reach: Vec3::new(0.0, 1.5, 0.0),
        period: 3.0,
        half_extents: Vec3::splat(0.6),
    },
    Bobbing {
        centre: Vec3::new(4.5, 4.0, -14.0),
        reach: Vec3::new(0.0, 1.5, 0.0),
        // Deliberately not the same, so the two are never in step and a screenshot shows two
        // different moments rather than one repeated.
        period: 2.1,
        half_extents: Vec3::splat(0.6),
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const CRATE: Bobbing = MOVING_CRATES[0];

    #[test]
    fn it_starts_in_the_middle_and_reaches_both_ends() {
        let at = |t: f32| CRATE.position_at(t);
        assert_eq!(at(0.0), CRATE.centre);
        assert!((at(CRATE.period * 0.25).y - (CRATE.centre.y + 1.5)).abs() < 1e-4);
        assert!((at(CRATE.period * 0.75).y - (CRATE.centre.y - 1.5)).abs() < 1e-4);
    }

    /// A pure function of the time: the same moment always gives the same place, whatever happened
    /// in between. That is what lets the server pause, or a test jump about, without the crate
    /// drifting away from where its history says it was.
    #[test]
    fn a_whole_period_later_it_is_back() {
        let start = CRATE.position_at(0.4);
        let later = CRATE.position_at(0.4 + CRATE.period * 5.0);
        assert!((start - later).length() < 1e-3);
    }

    /// A loose crate has to start clear of the floor and of the one below it, or the solver's first
    /// act is to push apart an overlap — which looks like an explosion and is nobody's intent.
    #[test]
    fn the_loose_crates_start_apart() {
        let size = LOOSE_HALF_EXTENT;
        assert!(LOOSE_CRATES[0].y > size, "the bottom crate starts inside the floor");
        for pair in LOOSE_CRATES.windows(2) {
            let gap = (pair[1] - pair[0]).abs();
            assert!(
                gap.x > size * 2.0 || gap.y > size * 2.0 || gap.z > size * 2.0,
                "two loose crates start overlapping: {:?} and {:?}",
                pair[0],
                pair[1],
            );
        }
    }

    /// It must clear a standing player at its lowest, or it is a crate you can walk through.
    #[test]
    fn it_stays_above_head_height() {
        for prop in MOVING_CRATES {
            let lowest = prop.centre.y - prop.reach.y - prop.half_extents.y;
            assert!(
                lowest > crate::movement::CAPSULE_Y_OFFSET + crate::movement::CAPSULE_HALF_HEIGHT,
                "a crate dips to {lowest} m, into a standing player",
            );
        }
    }

    /// The collider has to be the size the prop says it is. Avian measures a cuboid by its full
    /// side lengths and this type is written in half-extents, which is exactly how a crate ends up
    /// drawn at one size and shot at another.
    ///
    /// The shape a shot meets is this same collider — a `Hitbox` holds it rather than rebuilding
    /// one — so getting this right is the whole of getting the two to agree.
    #[test]
    fn the_collider_is_the_size_the_prop_claims() {
        let prop = MOVING_CRATES[0].prop();
        let aabb = prop.collider().aabb(Vec3::ZERO, Quat::IDENTITY);
        let half_extents = prop.half_extents;
        assert!((aabb.max - half_extents).length() < 1e-5, "{:?} vs {half_extents:?}", aabb.max);
        assert!((aabb.min + half_extents).length() < 1e-5, "{:?} vs {half_extents:?}", aabb.min);
    }
}
