//! Rewinding players to where the shooter saw them.
//!
//! Without this the server tests a shot against where a target is *now*, while the shooter aimed at
//! where the target was on their screen — which is older by the whole round trip **plus** the
//! interpolation delay. At 100 ms ping that is about 110 ms, and a player crossing at 6 m/s has
//! moved two thirds of a metre in it. You would have to lead a running target by most of its own
//! width, at every range, which is not a skill anyone wants to learn.
//!
//! The fix, in the shape Valve gave it: the server keeps a short history of every player's
//! position, and when a shot arrives it puts the world back to the moment the shooter was looking
//! at before testing the ray.
//!
//! Two things make that moment knowable. The shot travels as a *tick-stamped input*, so the server
//! knows exactly which tick the trigger was pulled on rather than when the packet happened to
//! arrive. And the client reports its own interpolation delay with every input message — that is
//! what `lag_compensation` in lightyear's `InputConfig` switches on — so the server knows how far
//! behind that tick the shooter's view of everyone else was.
//!
//! What it costs is the thing lag compensation always costs: you can be shot after stepping behind
//! a wall, because on the shooter's screen you had not stepped behind it yet. Every shooter makes
//! this trade. The alternative is that hitting a moving target requires leading it, which players
//! experience as the game being broken.
//!
//! Only *positions* are rewound, not the level: the geometry does not move, so a shot blocked by a
//! crate now was blocked by it then.

use bevy::prelude::*;
use lightyear::prelude::Tick;
use std::collections::VecDeque;

/// One recorded moment of a player: enough to rebuild their hitbox and nothing else.
///
/// Stance is in here because a crouched capsule is a different shape, and someone who ducked half a
/// round trip ago must still be standing in the past the shooter aimed at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub position: Vec3,
    pub crouching: bool,
}

/// A player's recent positions, oldest first.
///
/// One buffer per player rather than one snapshot of the whole world per tick. With hitscan against
/// a handful of capsules there is nothing to gain from the snapshot form — the ray asks about each
/// target separately anyway — and per-entity means a player who joins late simply has a short
/// history instead of a hole in a shared structure.
#[derive(Component, Debug, Default)]
pub struct PositionHistory {
    /// Ordered oldest-first, one entry per simulated tick.
    entries: VecDeque<(Tick, Snapshot)>,
    /// How many ticks are kept. Past this the oldest is dropped.
    capacity: usize,
}

impl PositionHistory {
    /// Keeps `ticks` of history. How far back a shot may reach is exactly this.
    pub fn with_capacity(ticks: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(ticks),
            capacity: ticks.max(1),
        }
    }

    /// Appends this tick's moment, dropping the oldest once the buffer is full.
    ///
    /// Recording a tick that is not newer than the last one overwrites from there instead of
    /// appending, so the buffer stays ordered whatever the caller does. The server does not roll
    /// back, so this should not happen — but a buffer that quietly went out of order would produce
    /// hits that are wrong in a way nothing else could explain.
    pub fn record(&mut self, tick: Tick, snapshot: Snapshot) {
        while self.entries.back().is_some_and(|(last, _)| *last >= tick) {
            self.entries.pop_back();
        }
        self.entries.push_back((tick, snapshot));
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }

    /// Where this player was at `tick`, plus `overstep` of the way to the next one.
    ///
    /// The overstep matters: the shooter's screen showed an interpolated position, somewhere
    /// *between* two received updates, not a recorded tick. Sampling only whole ticks would leave a
    /// residual error of up to one tick of movement — at 64 Hz and 6 m/s, about 9 cm, which is a
    /// quarter of a player's width.
    ///
    /// Returns `None` when the moment asked for is older than anything kept. Past the newest entry
    /// it clamps rather than extrapolating, for the same reason interpolation does: a guess about
    /// the future is how a player gets shot through a wall they were never near.
    pub fn sample(&self, tick: Tick, overstep: f32) -> Option<Snapshot> {
        // The last entry at or before `tick`.
        let index = self
            .entries
            .partition_point(|(recorded, _)| *recorded <= tick)
            .checked_sub(1)?;
        let (_, start) = self.entries[index];
        let Some((_, end)) = self.entries.get(index + 1).copied() else {
            return Some(start);
        };
        Some(Snapshot {
            position: start.position.lerp(end.position, overstep.clamp(0.0, 1.0)),
            // Discrete, so it holds until the moment actually arrives — half a crouch is not a
            // stance, and the same choice is made when interpolating remote players.
            crouching: start.crouching,
        })
    }

    /// The blend of two recorded ticks that a client was drawing, rebuilt from its own history.
    ///
    /// This is the exact form: the client reports the two confirmed ticks it was interpolating
    /// between and how far between them it was, and the same lerp over the same two ticks gives
    /// back the point that was on its screen. The delay-based [`Self::sample`] approximates it by
    /// picking a moment and blending the two ticks either side, which differs whenever the
    /// snapshots the client actually received were further apart than one tick — which, at any
    /// send rate below the tick rate, is always.
    ///
    /// Returns `None` if either end has been forgotten, for the same reason as [`Self::sample`]:
    /// half a bracket is not a bracket, and guessing the other half is how a shot lands on a
    /// position nobody was ever in.
    pub fn sample_bracket(&self, from: Tick, to: Tick, factor: f32) -> Option<Snapshot> {
        let start = self.at(from)?;
        let end = self.at(to)?;
        Some(Snapshot {
            position: start.position.lerp(end.position, factor.clamp(0.0, 1.0)),
            // Discrete, and held until the moment arrives — the same choice interpolation makes.
            crouching: start.crouching,
        })
    }

    /// The recorded moment at exactly `tick`, if it is still kept.
    fn at(&self, tick: Tick) -> Option<Snapshot> {
        let index = self
            .entries
            .binary_search_by_key(&tick, |(recorded, _)| *recorded)
            .ok()?;
        Some(self.entries[index].1)
    }

    /// True once the buffer has been filled, which is what separates "this player only just joined"
    /// from "the history is too short for this connection".
    pub fn is_full(&self) -> bool {
        self.entries.len() >= self.capacity
    }

    /// The oldest tick still kept, for reporting how far short a rewind fell.
    pub fn oldest(&self) -> Option<Tick> {
        self.entries.front().map(|(tick, _)| *tick)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten ticks of a player walking one metre per tick along +X.
    fn walking() -> PositionHistory {
        let mut history = PositionHistory::with_capacity(10);
        for tick in 0..10u32 {
            history.record(
                Tick(tick),
                Snapshot {
                    position: Vec3::new(tick as f32, 0.0, 0.0),
                    crouching: false,
                },
            );
        }
        history
    }

    #[test]
    fn a_whole_tick_is_returned_exactly() {
        let sample = walking().sample(Tick(4), 0.0).expect("tick 4 was recorded");
        assert_eq!(sample.position, Vec3::new(4.0, 0.0, 0.0));
    }

    #[test]
    fn the_overstep_lands_between_two_ticks() {
        let sample = walking().sample(Tick(4), 0.25).expect("tick 4 was recorded");
        assert_eq!(sample.position, Vec3::new(4.25, 0.0, 0.0));
    }

    /// The past is a different shape, not just a different place.
    #[test]
    fn stance_comes_from_the_past_too() {
        let mut history = PositionHistory::with_capacity(4);
        history.record(Tick(1), Snapshot { position: Vec3::ZERO, crouching: true });
        history.record(Tick(2), Snapshot { position: Vec3::ZERO, crouching: false });
        assert!(history.sample(Tick(1), 0.9).unwrap().crouching, "stance blended");
        assert!(!history.sample(Tick(2), 0.0).unwrap().crouching);
    }

    /// Asking past the newest entry must not invent movement that has not happened.
    #[test]
    fn the_present_clamps_instead_of_extrapolating() {
        let sample = walking().sample(Tick(99), 0.5).expect("clamped to the newest");
        assert_eq!(sample.position, Vec3::new(9.0, 0.0, 0.0));
    }

    /// A rewind further back than the buffer reaches must say so rather than return the oldest
    /// entry, which would silently be a hit against a position nobody was ever in.
    #[test]
    fn too_far_back_is_a_miss_not_a_guess() {
        let mut history = PositionHistory::with_capacity(4);
        for tick in 10..14u32 {
            history.record(Tick(tick), Snapshot { position: Vec3::ZERO, crouching: false });
        }
        assert!(history.sample(Tick(9), 0.0).is_none());
        assert!(history.sample(Tick(10), 0.0).is_some());
    }

    #[test]
    fn the_buffer_forgets_the_oldest_first() {
        let mut history = PositionHistory::with_capacity(3);
        for tick in 0..10u32 {
            history.record(Tick(tick), Snapshot { position: Vec3::ZERO, crouching: false });
        }
        assert_eq!(history.len(), 3);
        assert_eq!(history.oldest(), Some(Tick(7)));
        assert!(history.is_full());
    }

    /// The exact form: two confirmed ticks and a fraction, rebuilt from the dense history.
    #[test]
    fn a_bracket_is_rebuilt_from_both_ends() {
        let sample = walking().sample_bracket(Tick(2), Tick(6), 0.25).expect("both ends kept");
        assert_eq!(sample.position, Vec3::new(3.0, 0.0, 0.0));
    }

    /// A wide bracket is the normal case, not the exception: at a send rate below the tick rate the
    /// two snapshots a client received are several ticks apart, and blending them is not the same
    /// as reading the tick in the middle.
    #[test]
    fn a_wide_bracket_is_not_the_same_as_the_middle_tick() {
        let mut history = PositionHistory::with_capacity(8);
        for (tick, x) in [(0u32, 0.0), (1, 5.0), (2, 6.0)] {
            history.record(Tick(tick), Snapshot { position: Vec3::new(x, 0.0, 0.0), crouching: false });
        }
        // The client saw the blend of ticks 0 and 2, halfway: 3.0. The tick in between says 5.0.
        let blended = history.sample_bracket(Tick(0), Tick(2), 0.5).unwrap();
        assert_eq!(blended.position.x, 3.0);
        assert_eq!(history.sample(Tick(1), 0.0).unwrap().position.x, 5.0);
    }

    /// Half a bracket is not a bracket.
    #[test]
    fn a_forgotten_end_is_a_miss() {
        let mut history = PositionHistory::with_capacity(3);
        for tick in 10..13u32 {
            history.record(Tick(tick), Snapshot { position: Vec3::ZERO, crouching: false });
        }
        assert!(history.sample_bracket(Tick(8), Tick(12), 0.5).is_none());
        assert!(history.sample_bracket(Tick(10), Tick(99), 0.5).is_none());
        assert!(history.sample_bracket(Tick(10), Tick(12), 0.5).is_some());
    }

    /// A short history is not the same failure as a stale one, and the caller reports them
    /// differently: one is a player who just joined, the other a misconfiguration.
    #[test]
    fn a_fresh_history_is_not_full() {
        let mut history = PositionHistory::with_capacity(8);
        history.record(Tick(0), Snapshot { position: Vec3::ZERO, crouching: false });
        assert!(!history.is_full());
    }

    /// Re-recording a tick replaces it and everything after, rather than appending out of order.
    #[test]
    fn recording_backwards_rewrites_instead_of_corrupting() {
        let mut history = walking();
        history.record(Tick(5), Snapshot { position: Vec3::new(-1.0, 0.0, 0.0), crouching: false });
        assert_eq!(history.len(), 6);
        assert_eq!(history.sample(Tick(5), 0.0).unwrap().position.x, -1.0);
        assert_eq!(history.sample(Tick(9), 0.0).unwrap().position.x, -1.0, "clamped to the newest");
    }
}
