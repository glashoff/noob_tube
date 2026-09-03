//! Taking placements from clients, deciding whether they stand, and telling everybody.
//!
//! The same shape as [`sculpting`](crate::sculpting) and for the same reason — a gesture is sent as
//! what was asked for, and the server sends back what it decided — but with one deliberate
//! difference: **there is no commit tick.** A marker has no collider, so nothing it does can change
//! where a player may stand, and there is no per-tile rebuild to make idempotent. terrain.md §7
//! calls that exemption out, and it is why a placement appears the moment the server has seen it
//! rather than a tenth of a second later.
//!
//! What the server owes on validation is the list in §7, and each item on it is a different way for
//! a map to become something nobody can play:
//!
//! 1. **A kind the palette has.** A marker naming something the server cannot spawn is a load error
//!    later, in a place with less context than here.
//! 2. **A place on the map**, a bounded height above it, and a rotation that is finite and
//!    normalised. An unnormalised quaternion off the wire does not error, it scales and skews
//!    whatever it is applied to.
//! 3. **A cap on how many**, and a rate limit on how fast.
//! 4. **At least one player spawn survives.** A map with none is unplayable, and this is the
//!    cheapest place in the whole system that still knows.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::level;
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::terrain::{
    Ground, MAX_MARKERS, MARKER_EDITS_PER_SECOND, MARKER_EDIT_BURST, MapFault, MapList, Marker,
    MarkerChanged, MarkerEdit,
};

use crate::maps::Maps;

/// How many edits each peer may still make.
///
/// A bucket rather than a cooldown, the same choice sculpting makes and for a weaker version of the
/// same reason: laying out a row of crates is a burst, and a limit that refused the second one in a
/// hundred milliseconds would refuse the gesture rather than the abuse.
#[derive(Resource, Default)]
pub struct Placements(HashMap<PeerId, f32>);

/// Sending to clients, which always takes both halves.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Wire<'w, 's> {
    sender: ServerMultiMessageSender<'w, 's>,
    server: Single<'w, 's, &'static Server>,
}

/// PreUpdate: applies the placements it accepts and refuses the rest.
///
/// Ordered with the rest of the map's traffic, after a map switch and beside sculpting: an edit
/// aimed at the map that is going away is checked against the map that has arrived, which is the
/// conservative direction — the new map's footprint is the one a marker has to stand inside.
pub fn serve_marker_edits(
    mut inbox: Query<(&RemoteId, &mut MessageReceiver<MarkerEdit>)>,
    mut ground: ResMut<Ground>,
    mut placements: ResMut<Placements>,
    mut maps: ResMut<Maps>,
    time: Res<Time<Real>>,
    mut wire: Wire,
) {
    let Wire { sender, server } = &mut wire;
    let refill = MARKER_EDITS_PER_SECOND * time.delta_secs();
    for budget in placements.0.values_mut() {
        *budget = (*budget + refill).min(MARKER_EDIT_BURST);
    }

    let mut asked: Vec<(PeerId, MarkerEdit)> = Vec::new();
    for (remote, mut receiver) in inbox.iter_mut() {
        for edit in receiver.receive() {
            asked.push((remote.0, edit));
        }
    }

    for (peer, edit) in asked {
        // Charged before it is judged, so that a client sending nothing but rubbish still pays for
        // the sending. The bucket is the only guard that cares how *often*, and a refusal that
        // cost nothing would be a refusal worth spamming.
        let budget = placements.0.entry(peer).or_insert(MARKER_EDIT_BURST);
        let refused = if *budget < 1.0 {
            Some(MapFault::TooManyEdits)
        } else {
            *budget -= 1.0;
            judge(&ground.0, &edit).err()
        };

        if let Some(fault) = refused {
            // Answered rather than dropped: a client whose placement went nowhere has to be able to
            // tell a refusal from a lost packet, and this channel is reliable so that it never has
            // to guess. It rides the map list, which is where every other refusal goes.
            let report = MapList { trouble: Some(fault.to_string()), ..maps.report(None) };
            if let Err(error) =
                sender.send::<_, TerrainChannel>(&report, **server, &NetworkTarget::Single(peer))
            {
                warn!("could not refuse a placement from {peer:?}: {error}");
            }
            continue;
        }

        let change = match edit {
            // The handle is assigned here and nowhere else, which is the whole reason the answer
            // is a different type from the question.
            MarkerEdit::Place(marker) => MarkerChanged::Placed(Marker {
                id: ground.0.next_marker_id(),
                rotation: marker.rotation.normalize(),
                ..marker
            }),
            MarkerEdit::Turn { id, rotation } => {
                MarkerChanged::Turned { id, rotation: rotation.normalize() }
            }
            MarkerEdit::Remove { id } => MarkerChanged::Removed { id },
        };

        // Everybody, including the placer: their client applies what the server accepted rather
        // than what it asked for, so there is no path where one machine has a marker the rest do
        // not.
        if let Err(error) = sender.send::<_, TerrainChannel>(&change, **server, &NetworkTarget::All)
        {
            error!("could not send a placement on: {error}");
            continue;
        }
        ground.0.apply(&change);
        // The map in play now differs from the file it came from, and the menu is the only place
        // that can say so. Nothing here resolves it: saving is explicit, always.
        maps.unsaved = true;
    }
}

/// Whether this edit is one the map will take.
///
/// Split out so that every arm is read side by side: the three verbs share a map and differ only in
/// which of its rules they can break.
fn judge(ground: &noob_tube_shared::terrain::Terrain, edit: &MarkerEdit) -> Result<(), MapFault> {
    match edit {
        MarkerEdit::Place(marker) => {
            if !level::PLACEABLES.contains(&marker.kind.as_str()) {
                return Err(MapFault::NoSuchKind);
            }
            if ground.markers.len() >= MAX_MARKERS {
                return Err(MapFault::TooManyMarkers);
            }
            // Against a normalised copy, because that is what would be stored: a quaternion that is
            // merely sloppy is fixed rather than refused, and one that cannot be normalised at all
            // — zero, or not finite — fails here.
            Marker { rotation: marker.rotation.normalize(), ..marker.clone() }.check(ground.grid)
        }
        MarkerEdit::Turn { id, rotation } => {
            let marker = ground
                .markers
                .iter()
                .find(|marker| marker.id == *id)
                .ok_or(MapFault::NoSuchMarker)?;
            Marker { rotation: rotation.normalize(), ..marker.clone() }.check(ground.grid)
        }
        MarkerEdit::Remove { id } => {
            if !ground.markers.iter().any(|marker| marker.id == *id) {
                return Err(MapFault::NoSuchMarker);
            }
            if ground.is_the_last_spawn(*id) {
                return Err(MapFault::LastSpawn);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::terrain::{MAX_MARKER_Y, Terrain, default_terrain};

    fn a_crate_at(terrain: &Terrain, x: f32, z: f32) -> MarkerEdit {
        MarkerEdit::Place(Marker {
            id: terrain.next_marker_id(),
            kind: level::CRATE.into(),
            x,
            z,
            y: 0.0,
            rotation: Quat::IDENTITY,
        })
    }

    /// The ordinary case, so that the refusals below mean something.
    #[test]
    fn a_crate_on_the_map_is_allowed() {
        let terrain = default_terrain();
        assert_eq!(judge(&terrain, &a_crate_at(&terrain, 20.0, -30.0)), Ok(()));
    }

    /// Every kind the palette offers is one the server will actually take.
    ///
    /// The palette is what a client puts in its hotbar, and this is what decides whether a click
    /// stands. They are two lists in two crates, and the day they disagree is the day a slot
    /// refuses every placement with the refusal arriving from the far end of a network.
    #[test]
    fn the_palette_is_the_list_of_what_can_be_placed() {
        let terrain = default_terrain();
        for kind in level::PLACEABLES {
            let MarkerEdit::Place(marker) = a_crate_at(&terrain, 10.0, -10.0) else {
                unreachable!()
            };
            let asked = MarkerEdit::Place(Marker { kind: kind.into(), ..marker });
            assert_eq!(judge(&terrain, &asked), Ok(()), "the palette offers {kind}");
        }
    }

    /// Every way a placement can be refused, side by side.
    ///
    /// Together rather than one test each, because the point is the *set*: each of these is a
    /// different way for a map to become one nobody can play, and a gap in the list is invisible
    /// until somebody finds it from the outside.
    #[test]
    fn a_placement_has_to_be_one_the_map_can_hold() {
        let terrain = default_terrain();
        let (_, high) = terrain.grid.bounds();

        let unknown = MarkerEdit::Place(Marker {
            kind: "dragon".into(),
            ..match a_crate_at(&terrain, 0.0, 0.0) {
                MarkerEdit::Place(marker) => marker,
                _ => unreachable!(),
            }
        });
        assert_eq!(judge(&terrain, &unknown), Err(MapFault::NoSuchKind));

        let off_the_map = a_crate_at(&terrain, high.x + 10.0, 0.0);
        assert_eq!(judge(&terrain, &off_the_map), Err(MapFault::Marker));

        let MarkerEdit::Place(mut marker) = a_crate_at(&terrain, 0.0, 0.0) else { unreachable!() };
        marker.y = MAX_MARKER_Y + 1.0;
        assert_eq!(judge(&terrain, &MarkerEdit::Place(marker.clone())), Err(MapFault::Marker));

        // A quaternion that cannot be normalised at all. One that is merely sloppy is fixed rather
        // than refused, which is the next test.
        marker.y = 0.0;
        marker.rotation = Quat::from_xyzw(0.0, 0.0, 0.0, 0.0);
        assert_eq!(judge(&terrain, &MarkerEdit::Place(marker)), Err(MapFault::Marker));
    }

    /// A rotation nobody normalised is fixed on the way in, not refused.
    ///
    /// It does not error when it is used — it scales and skews whatever it is applied to — so the
    /// server renormalises rather than trusting the sender, and only a quaternion with no direction
    /// in it at all is turned away.
    #[test]
    fn a_sloppy_rotation_is_taken_and_tidied() {
        let terrain = default_terrain();
        let MarkerEdit::Place(mut marker) = a_crate_at(&terrain, 4.0, 4.0) else { unreachable!() };
        marker.rotation = Quat::from_xyzw(0.0, 0.6, 0.0, 0.6);
        assert_eq!(judge(&terrain, &MarkerEdit::Place(marker)), Ok(()));
    }

    /// An edit naming a marker that is not there is refused rather than ignored.
    #[test]
    fn an_edit_has_to_name_a_marker_the_map_has() {
        let terrain = default_terrain();
        let turn = MarkerEdit::Turn { id: 9999, rotation: Quat::IDENTITY };
        assert_eq!(judge(&terrain, &turn), Err(MapFault::NoSuchMarker));
        assert_eq!(
            judge(&terrain, &MarkerEdit::Remove { id: 9999 }),
            Err(MapFault::NoSuchMarker),
        );
    }

    /// The map keeps a way in, whatever anybody asks for.
    ///
    /// A map with no player spawn is unplayable, and this handler is the cheapest place in the
    /// system that still knows enough to say no. Deleting the second-to-last is fine; deleting the
    /// last is not.
    #[test]
    fn the_last_player_spawn_cannot_be_deleted() {
        let mut terrain = default_terrain();
        let spawns: Vec<u32> =
            terrain.markers_of(level::PLAYER_SPAWN).map(|marker| marker.id).collect();
        for id in spawns.iter().skip(1) {
            assert_eq!(judge(&terrain, &MarkerEdit::Remove { id: *id }), Ok(()));
            terrain.apply(&MarkerChanged::Removed { id: *id });
        }
        assert_eq!(
            judge(&terrain, &MarkerEdit::Remove { id: spawns[0] }),
            Err(MapFault::LastSpawn),
        );
        // And everything else on the map is still removable, which is what says the rule is about
        // spawns rather than about the list running low.
        let crate_id = terrain.markers_of(level::CRATE).next().expect("a crate").id;
        assert_eq!(judge(&terrain, &MarkerEdit::Remove { id: crate_id }), Ok(()));
    }

    /// A map fills up, and says so before it does.
    #[test]
    fn a_map_stops_taking_markers_at_its_cap() {
        let mut terrain = default_terrain();
        while terrain.markers.len() < MAX_MARKERS {
            let id = terrain.next_marker_id();
            terrain.markers.push(Marker {
                id,
                kind: level::CRATE.into(),
                x: 0.0,
                z: 0.0,
                y: 0.0,
                rotation: Quat::IDENTITY,
            });
        }
        assert_eq!(judge(&terrain, &a_crate_at(&terrain, 0.0, 0.0)), Err(MapFault::TooManyMarkers));
    }
}
