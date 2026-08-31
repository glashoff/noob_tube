//! Things in the world that move but are nobody's player.
//!
//! There is one kind so far: a crate that rides up and down. It exists to be shot at while moving,
//! which is the case lag compensation has to cover for anything that is not a player — a lift, a
//! swinging door, a train.
//!
//! **The animation runs on the server alone.** Clients receive [`Hitbox`] like any other replicated
//! component and interpolate it; nothing on a client works out where the crate ought to be. That is
//! deliberate even though the motion is a pure function of the tick and every client *could* compute
//! it: the moment anything can stop, push or break the crate, a locally computed one is wrong, and
//! the version that is wrong later is not worth being right now. It also means the crate goes down
//! exactly the same path as a player — replicated, interpolated, and rewound out of a
//! [`HitboxHistory`](crate::lag_compensation::HitboxHistory) — instead of a second mechanism beside
//! it.
//!
//! What it is *not* is terrain. It stops bullets, because a shot tests every hitbox and takes the
//! nearest, but it does not stop feet: the level's collision geometry is a static BVH built once,
//! with no way to move a collider in it. Standing on a lift is a separate piece of work.

use bevy::prelude::*;

use crate::hitbox::Hitbox;

/// A prop that slides back and forth along one axis, forever.
///
/// Server-only, and never replicated: what reaches a client is the [`Hitbox`] this produces.
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
    /// Where the prop is at a moment, as the shape a shot would meet.
    ///
    /// A pure function of the time, not an accumulation, so the crate cannot drift: a server that
    /// paused for a second comes back where it should be rather than a second behind.
    pub fn hitbox_at(&self, seconds: f32) -> Hitbox {
        let phase = core::f32::consts::TAU * seconds / self.period.max(f32::MIN_POSITIVE);
        Hitbox::Prop {
            centre: self.centre + self.reach * phase.sin(),
            half_extents: self.half_extents,
        }
    }
}

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
        let at = |t: f32| CRATE.hitbox_at(t).centre();
        assert_eq!(at(0.0), CRATE.centre);
        assert!((at(CRATE.period * 0.25).y - (CRATE.centre.y + 1.5)).abs() < 1e-4);
        assert!((at(CRATE.period * 0.75).y - (CRATE.centre.y - 1.5)).abs() < 1e-4);
    }

    /// A pure function of the time: the same moment always gives the same place, whatever happened
    /// in between. That is what lets the server pause, or a test jump about, without the crate
    /// drifting away from where its history says it was.
    #[test]
    fn a_whole_period_later_it_is_back() {
        let start = CRATE.hitbox_at(0.4);
        let later = CRATE.hitbox_at(0.4 + CRATE.period * 5.0);
        assert!((start.centre() - later.centre()).length() < 1e-3);
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
}
