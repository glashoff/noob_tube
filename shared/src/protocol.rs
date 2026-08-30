//! What travels over the wire.
//!
//! Lightyear replicates *components*, not snapshots. There is no per-tick blob containing the whole
//! world: each component is registered on its own and gets its own treatment — replicated,
//! predicted, interpolated, or a combination. Both sides must register the same set in the same
//! order, which is why this lives in `shared` and is added by both binaries.
//!
//! Registration has to happen after the client or server plugin group and before any `Client` or
//! `Server` entity is spawned.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::input::native::InputPlugin;
use std::f32::consts::{PI, TAU};

use crate::player::{Aim, Player, PlayerInput, PlayerState};
use crate::shooting::{Health, ShotFired};
use crate::tuning::NetConfig;
use crate::types::SharedTypesPlugin;

/// Carries the tuning both sides need at registration time — the input rate and redundancy are
/// part of how the input plugin is built, not something that can be changed later.
pub struct ProtocolPlugin {
    pub net: NetConfig,
}

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SharedTypesPlugin);
        // Position and velocity. The server is the authority; the owning client predicts it and
        // rolls back when the two disagree.
        //
        // Prediction and interpolation are both registered, and which one an entity gets is decided
        // per entity by the marker it carries — `Predicted` for its owner, `Interpolated` for
        // everyone else. The server hands those out with `PredictionTarget` and
        // `InterpolationTarget`, so nothing here decides it.
        app.component::<PlayerState>()
            .replicate()
            .predict()
            .add_interpolation_with(lerp_player_state);
        // Aim is separate so it can be treated differently — see the note on `PlayerState`.
        //
        // It is predicted alongside position rather than left to arrive late. The camera does not
        // read it (mouse look owns its own angles, which no rollback may touch), but the body does,
        // and an unpredicted `Aim` on a predicted entity means replication and the movement step
        // both writing it every tick with values a round trip apart.
        app.component::<Aim>()
            .replicate()
            .predict()
            .add_interpolation_with(lerp_aim);
        // Sent once per entity: which peer this player belongs to never changes.
        app.component::<Player>().replicate_once();
        // Health is the server's alone. A client predicting whether its shot landed would have to
        // un-kill someone on screen when the server disagreed, and there is no graceful way to do
        // that — so this only ever arrives.
        app.component::<Health>().replicate();

        // Inputs travel the other way, client to server. The plugin sends the last N packets'
        // worth of ticks with every message rather than one input per message, so a dropped packet
        // does not cost a tick of movement — and it keeps the history a rollback replays from.
        //
        // How often and how redundantly is `cmd_hz` and `input_redundancy`; both are fixed here,
        // at registration, which is why the protocol needs the config at all.
        app.add_plugins(InputPlugin::<PlayerInput> {
            config: self.net.input_config(),
        });

        // Shots, going the other way: the server tells everyone what it resolved so they can draw
        // it. A message rather than a component, because a shot is an event — it happens once and
        // has no state afterwards, and replicating a component would mean inventing an entity to
        // hang it on and then deciding when to remove it.
        //
        // Unreliable on purpose. A tracer lives for a twentieth of a second, so a retransmitted one
        // would arrive after the moment it belongs to; drawing it then is worse than not drawing it.
        // The direction matters twice over: it is what wires the channel into each connection's
        // transport, not merely a declaration. Registering the channel without it leaves the server
        // sending into a `ChannelNotFound`, logged once per shot and dropped.
        app.add_channel::<EffectsChannel>(ChannelSettings {
            mode: ChannelMode::UnorderedUnreliable,
            ..default()
        })
        .add_direction(NetworkDirection::ServerToClient);
        app.register_message::<ShotFired>()
            .add_direction(NetworkDirection::ServerToClient);
    }
}

/// The channel everything cosmetic travels on.
///
/// Separate from replication so that a burst of effects can never delay a position update, and so
/// that the whole lot can be dropped under bandwidth pressure without losing anything the
/// simulation depends on.
pub struct EffectsChannel;

/// Blends two received player states for a moment in between them.
///
/// Lightyear samples this on the interpolation timeline, which trails the last received update by
/// about one and a half send intervals. `t` is where in that gap we are, 0 at `start` and 1 at
/// `end`; it is never extrapolated past 1, so a late packet freezes a player rather than sending
/// them sliding through a wall.
///
/// Only the continuous fields can actually be blended. `on_ground` and `crouching` are discrete,
/// and half a crouch is not a stance — they hold `start`'s value until the timeline reaches `end`,
/// which is the choice that never shows a state before it happened.
fn lerp_player_state(start: PlayerState, end: PlayerState, t: f32) -> PlayerState {
    PlayerState {
        position: start.position.lerp(end.position, t),
        velocity: start.velocity.lerp(end.velocity, t),
        on_ground: start.on_ground,
        crouching: start.crouching,
        fire_cooldown: start.fire_cooldown,
    }
}

/// Blends two received aim angles.
fn lerp_aim(start: Aim, end: Aim, t: f32) -> Aim {
    Aim {
        yaw: lerp_angle(start.yaw, end.yaw, t),
        pitch: lerp_angle(start.pitch, end.pitch, t),
    }
}

/// Interpolates two angles the short way around the circle.
///
/// Yaw is unbounded and wraps, so a player turning past π reports something near +π on one tick and
/// near −π on the next. Interpolating those numbers directly would spin the body almost all the way
/// round in one send interval, in the wrong direction, on every crossing.
///
/// The result is an angle, not a number in any particular range: walking from 2.0 to −3.0 lands on
/// 3.28, which is the same direction. Everything downstream feeds it to `Quat::from_rotation_*`,
/// which cannot tell the difference; normalising would only introduce a discontinuity of its own.
fn lerp_angle(start: f32, end: f32, t: f32) -> f32 {
    let mut delta = (end - start).rem_euclid(TAU);
    if delta > PI {
        delta -= TAU;
    }
    start + delta * t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tolerance in radians. The angle maths goes through `rem_euclid`, so exact equality is not
    /// on offer, but anything a player could see is orders of magnitude larger than this.
    const EPS: f32 = 1e-5;

    #[test]
    fn halfway_is_halfway() {
        assert!((lerp_angle(0.0, 1.0, 0.5) - 0.5).abs() < EPS);
    }

    /// Compares two angles as directions rather than as numbers.
    fn same_direction(a: f32, b: f32) -> bool {
        let mut delta = (a - b).rem_euclid(TAU);
        if delta > PI {
            delta -= TAU;
        }
        delta.abs() < EPS
    }

    /// The ends have to land on the samples exactly, or a player would never quite face where the
    /// server says they face. Exactly *as a direction*: crossing the seam takes the number out of
    /// [−π, π], on purpose.
    #[test]
    fn the_ends_are_exact() {
        assert!((lerp_angle(2.0, -3.0, 0.0) - 2.0).abs() < EPS);
        assert!(same_direction(lerp_angle(2.0, -3.0, 1.0), -3.0));
        assert!(same_direction(lerp_angle(0.5, 1.5, 1.0), 1.5));
    }

    /// The case the whole function exists for: crossing the seam at ±π.
    #[test]
    fn crossing_the_seam_takes_the_short_way() {
        let start = PI - 0.1;
        let end = -PI + 0.1;
        let middle = lerp_angle(start, end, 0.5);
        // The short way is 0.2 rad forward, putting the midpoint just past π rather than at 0.
        let stepped = (middle - start).abs();
        assert!(stepped < 0.2, "turned {stepped} rad instead of 0.1");
    }

    #[test]
    fn a_turn_of_half_a_circle_does_not_reverse() {
        // Exactly π is the ambiguous case; either direction is equally short. It must at least
        // move by half of it, not stand still or jump.
        let middle = lerp_angle(0.0, PI, 0.5);
        assert!((middle.abs() - PI / 2.0).abs() < EPS, "midpoint was {middle}");
    }

    #[test]
    fn position_blends_but_stance_does_not() {
        let start = PlayerState {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            on_ground: true,
            crouching: false,
            fire_cooldown: 0,
        };
        let end = PlayerState {
            position: Vec3::new(4.0, 0.0, 0.0),
            velocity: Vec3::new(8.0, 0.0, 0.0),
            on_ground: false,
            crouching: true,
            fire_cooldown: 4,
        };

        let middle = lerp_player_state(start, end, 0.5);
        assert_eq!(middle.position, Vec3::new(2.0, 0.0, 0.0));
        assert_eq!(middle.velocity, Vec3::new(4.0, 0.0, 0.0));
        assert!(middle.on_ground, "stance changed before reaching the sample");
        assert!(!middle.crouching, "stance changed before reaching the sample");
    }
}
