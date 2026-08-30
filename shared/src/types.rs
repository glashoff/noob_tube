//! Types both sides share with the tooling, and the plugin that registers them.
//!
//! Registration is unconditional. Reflection costs nothing at runtime, and both the remote protocol
//! and the inspector are blind to anything that is not registered — gating it behind a feature would
//! only mean that whichever tool is enabled sees half the world.

use bevy::prelude::*;

use crate::player::{PlayerInput, PlayerState};

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
pub struct SharedTypesPlugin;

impl Plugin for SharedTypesPlugin {
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
            .register_type::<PlayerState>();
    }
}
