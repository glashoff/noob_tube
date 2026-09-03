//! Taking the water level from clients, and telling everybody where the sea is now.
//!
//! The smallest of the three edit handlers, and the same shape as
//! [`markers`](crate::markers): a client asks, the server judges, and what the server accepted goes
//! to everybody including the asker. Two things make it smaller than either of its siblings.
//!
//! **There is no commit tick.** Water is a picture today — a surface to look at, with no collider
//! and nothing that decides where a player may stand — so nothing it does can straddle a rollback
//! window. That is the exemption terrain.md §7 spells out for placement, claimed here for the same
//! reason and not a weaker one. The day water floats a vehicle or slows a swimmer, this handler
//! grows a tick and the sculpting one is the model for it.
//!
//! **There is nothing to charge for.** A stroke costs area and a marker costs a list entry; a water
//! level costs one `f32` in a struct that already has the field. What is left worth guarding is how
//! *often* a client may send one, because a client in a loop is a broadcast to every peer in the
//! game — so the bucket is the marker bucket's numbers, for a gesture that is made at the same
//! human pace.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::terrain::{
    Ground, MARKER_EDITS_PER_SECOND, MARKER_EDIT_BURST, MapFault, MapList, WaterLevel,
};

use crate::maps::Maps;

/// How many water edits each peer may still make.
///
/// Its own bucket rather than a share of the marker one, because they are separate gestures with
/// separate rates: nudging the waterline with the wheel while laying out a row of crates should
/// not run either of them dry. The *numbers* are shared, since both are limits on a hand rather
/// than on a cost.
#[derive(Resource, Default)]
pub struct WaterEdits(HashMap<PeerId, f32>);

/// Sending to clients, which always takes both halves.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Wire<'w, 's> {
    sender: ServerMultiMessageSender<'w, 's>,
    server: Single<'w, 's, &'static Server>,
}

/// PreUpdate: sets the water level, and refuses one the map cannot hold.
///
/// Ordered with the rest of the map's traffic and after a map switch, so a level aimed at the map
/// that is going away is checked against the map that has arrived — the conservative direction,
/// since it is the new map's height range the water has to sit inside.
pub fn serve_water_edits(
    mut inbox: Query<(&RemoteId, &mut MessageReceiver<WaterLevel>)>,
    mut ground: ResMut<Ground>,
    mut edits: ResMut<WaterEdits>,
    mut maps: ResMut<Maps>,
    time: Res<Time<Real>>,
    mut wire: Wire,
) {
    let Wire { sender, server } = &mut wire;
    let refill = MARKER_EDITS_PER_SECOND * time.delta_secs();
    for budget in edits.0.values_mut() {
        *budget = (*budget + refill).min(MARKER_EDIT_BURST);
    }

    let mut asked: Vec<(PeerId, WaterLevel)> = Vec::new();
    for (remote, mut receiver) in inbox.iter_mut() {
        for level in receiver.receive() {
            asked.push((remote.0, level));
        }
    }

    for (peer, level) in asked {
        // Charged before it is judged, so that a client sending nothing but rubbish still pays for
        // the sending — a refusal that cost nothing is a refusal worth spamming.
        let budget = edits.0.entry(peer).or_insert(MARKER_EDIT_BURST);
        let refused = if *budget < 1.0 {
            Some(MapFault::TooManyEdits)
        } else {
            *budget -= 1.0;
            level.check(&ground.0.grid).err()
        };

        if let Some(fault) = refused {
            // Answered rather than dropped, for the reason every other refusal here is: a client
            // whose edit went nowhere has to be able to tell a refusal from a lost packet, and this
            // channel is reliable so that it never has to guess.
            let report = MapList { trouble: Some(fault.to_string()), ..maps.report(None) };
            if let Err(error) =
                sender.send::<_, TerrainChannel>(&report, **server, &NetworkTarget::Single(peer))
            {
                warn!("could not refuse a water level from {peer:?}: {error}");
            }
            continue;
        }

        // Everybody, including the sender: their client has already put the water where it asked
        // for it, and this is what makes that guess the map's own answer rather than one machine's.
        if let Err(error) = sender.send::<_, TerrainChannel>(&level, **server, &NetworkTarget::All)
        {
            error!("could not send a water level on: {error}");
            continue;
        }
        match level.0 {
            Some(y) => info!("the water is at {y:.2} m"),
            None => info!("the map is dry"),
        }
        // Through `ResMut` on purpose: touching it is what tells the ground and the props there is
        // something to rebuild, and on a client it is what redraws the surface.
        ground.0.water_y = level.0;
        // The map in play now differs from the file it came from, and the menu is the only place
        // that can say so. Nothing here resolves it: saving is explicit, always.
        maps.unsaved = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::terrain::default_terrain;

    /// The range check, which is the whole of the judging this handler does.
    ///
    /// Together in one test because the point is the *set*: a level inside the map is taken, a dry
    /// map is a level like any other, and the two ways of naming a place no map has are refused.
    #[test]
    fn the_water_has_to_sit_somewhere_the_map_reaches() {
        let grid = default_terrain().grid;
        assert_eq!(WaterLevel(Some(0.0)).check(&grid), Ok(()));
        assert_eq!(WaterLevel(None).check(&grid), Ok(()), "a dry map is an edit, not a refusal");
        assert_eq!(WaterLevel(Some(grid.min_y)).check(&grid), Ok(()), "the floor is reachable");
        assert_eq!(WaterLevel(Some(grid.max_y)).check(&grid), Ok(()), "and so is the ceiling");
        assert_eq!(WaterLevel(Some(grid.max_y + 1.0)).check(&grid), Err(MapFault::Water));
        assert_eq!(WaterLevel(Some(f32::NAN)).check(&grid), Err(MapFault::Water));
    }
}
