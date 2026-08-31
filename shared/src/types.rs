//! Types both sides share with the tooling, and the plugin that registers them.
//!
//! Registration is unconditional. Reflection costs nothing at runtime, and both the remote protocol
//! and the inspector are blind to anything that is not registered — gating it behind a feature would
//! only mean that whichever tool is enabled sees half the world.

use bevy::prelude::*;

use crate::player::{Aim, Player, PlayerInput, PlayerState};
use crate::shooting::Health;

/// Marks an entity spawned by this project, as opposed to one a Bevy plugin created.
///
/// There is no such distinction in the ECS itself. `DefaultPlugins` is not a library running
/// alongside us; it is roughly forty plugins whose `build` functions write into the same world ours
/// do, so a client holds some 563 entities of which a handful are ours — observers, one entity per
/// resource, one per registered remote method, three placeholders for gizmo draw phases.
///
/// Filtering on `Name` came close, because we name what we spawn, but it is a coincidence rather
/// than a rule: `bevy_gizmos_render` names its three placeholders too, and they show up looking like
/// ours. This marker says it outright.
///
/// It has to be added by hand at every spawn site, which means it can be forgotten. That is the
/// price of not having Bevy tell us who created an entity — it does not track that.
#[derive(Component, Reflect, Default, Clone, Copy, Debug)]
#[reflect(Component)]
pub struct Authored;

/// Registers the shared types for reflection.
///
/// Deliberately not unique. Several plugins want these registrations and none of them can know
/// whether another already added it — the protocol needs them, so does the remote endpoint, and
/// either can be present without the other. Bevy panics on a duplicate plugin by default, which
/// turned a working server into a crash the moment both were enabled. Since every statement here is
/// an idempotent `register_type`, running twice is harmless, and saying so is better than guarding
/// at every call site and missing one.
pub struct SharedTypesPlugin;

impl Plugin for SharedTypesPlugin {
    fn is_unique(&self) -> bool {
        false
    }

    fn build(&self, app: &mut App) {
        app
            // A headless server built on MinimalPlugins has almost nothing in its registry — not
            // even `Name`, without which an entity listing is bare ids.
            .register_type::<Name>()
            // Field types have to be registered too, or a component that reflects fine still fails
            // to serialise.
            .register_type::<Vec2>()
            .register_type::<Vec3>()
            .register_type::<Authored>()
            .register_type::<PlayerInput>()
            .register_type::<PlayerState>()
            .register_type::<Aim>()
            .register_type::<Player>()
            .register_type::<Health>()
            .register_type::<crate::props::Bobbing>()
            .register_type::<crate::props::Prop>()
            // A vehicle's wheels are worth registering even though they never travel: they are
            // derived every tick from the pose and the ground, so a live listing of them is how one
            // sees what the suspension thinks it is doing.
            .register_type::<crate::vehicle::Wheel>()
            .register_type::<crate::vehicle::Wheels>()
            .register_type::<crate::vehicle::VehicleKind>()
            .register_type::<crate::vehicle::Controls>()
            .register_type::<crate::vehicle::Righting>()
            .register_type::<crate::vehicle::Driving>();
    }
}
