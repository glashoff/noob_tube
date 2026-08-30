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

use crate::player::{Aim, Player, PlayerInput, PlayerState};
use crate::types::SharedTypesPlugin;

pub struct ProtocolPlugin;

impl Plugin for ProtocolPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(SharedTypesPlugin);
        // Position and velocity: the server is the authority, and in M4 this is what the client
        // predicts and rolls back.
        app.component::<PlayerState>().replicate();
        // Aim is separate so it can be treated differently — see the note on `PlayerState`.
        app.component::<Aim>().replicate();
        // Sent once per entity: which peer this player belongs to never changes.
        app.component::<Player>().replicate_once();

        // Inputs travel the other way, client to server. The plugin sends the last N ticks with
        // every packet rather than one input per packet, so a dropped packet does not cost a tick
        // of movement — and it keeps the history the client needs to replay after a rollback.
        app.add_plugins(InputPlugin::<PlayerInput>::default());
    }
}
