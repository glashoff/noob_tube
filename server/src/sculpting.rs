//! Taking strokes from clients, deciding when they land, and telling everybody.
//!
//! The server does three things a client cannot be trusted to do for itself, and each of them is
//! the reason a stroke goes through here rather than straight into the ground:
//!
//! 1. **It refuses a stroke that is not one.** Radius, coordinates and every brush parameter are
//!    checked for being finite and within their caps before anything is allocated or charged.
//! 2. **It charges for the area.** A token bucket over touched samples, on an upper bound worked
//!    out before a single sample is written. Undercharging would let a client buy a bigger stroke
//!    than it pays for — and since the commit is broadcast before it is applied, an oversized
//!    stroke stalls every machine in the game rather than only the sender's.
//! 3. **It says when.** Every accepted stroke is stamped `now + max_predicted_ticks`, which is what
//!    keeps a rollback window from ever straddling an edit. See [`sculpt`](noob_tube_shared::sculpt)
//!    for why that matters and what it costs.

use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::sculpt::{PendingEdits, SAMPLE_BURST, SAMPLES_PER_SECOND, Stroke, TerrainEdit};
use noob_tube_shared::terrain::{Ground, MapFault, MapList};
use noob_tube_shared::tuning::NetConfig;

use crate::maps::Maps;

/// The three things a stroke changes: what a peer may still spend, what is waiting for its tick,
/// and whether the map in play still matches its file.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Books<'w> {
    budgets: ResMut<'w, Budgets>,
    pending: ResMut<'w, PendingEdits>,
    maps: ResMut<'w, Maps>,
}

/// Sending to clients, which always takes both halves.
#[derive(bevy::ecs::system::SystemParam)]
pub struct Wire<'w, 's> {
    sender: ServerMultiMessageSender<'w, 's>,
    server: Single<'w, 's, &'static Server>,
}

/// How much ground each peer may still write.
///
/// A bucket rather than a cooldown, because sculpting is *held*: the natural gesture is a stream of
/// overlapping strokes, and anything that refuses the second one in a hundred milliseconds refuses
/// sculpting itself. What it limits is area, which is the thing that costs.
#[derive(Resource, Default)]
pub struct Budgets(HashMap<PeerId, f32>);

/// FixedUpdate is not where this belongs: PreUpdate, before the ground is rebuilt and before
/// `FixedMain` steps anybody on it.
///
/// Ordered after the map requests, so a stroke and a map switch arriving in the same frame are
/// resolved the way they were sent — a stroke aimed at the map that is going away is refused
/// against the map that has arrived, which is the conservative direction: the caps of the new map
/// are the ones its samples have to satisfy.
pub fn serve_strokes(
    mut inbox: Query<(&RemoteId, &mut MessageReceiver<Stroke>)>,
    mut books: Books,
    ground: Res<Ground>,
    net: Res<NetConfig>,
    time: Res<Time<Real>>,
    timeline: Res<LocalTimeline>,
    mut wire: Wire,
) {
    let Books { budgets, pending, maps } = &mut books;
    let (budgets, pending, maps) = (&mut **budgets, &mut **pending, &mut **maps);
    let Wire { sender, server } = &mut wire;
    // Everybody's bucket refills, whether or not they are sculpting. Capped at the burst, so time
    // spent not sculpting banks a little and not an afternoon.
    let refill = SAMPLES_PER_SECOND * time.delta_secs();
    for budget in budgets.0.values_mut() {
        *budget = (*budget + refill).min(SAMPLE_BURST);
    }

    let grid = ground.0.grid;
    // `now` is read once, so every stroke in a frame lands on the same tick. Two strokes a frame
    // apart on the same spot are already ordered by the queue; giving them different ticks would
    // only add a tick of stutter to the second.
    let due = timeline.tick().0 + u32::from(net.max_predicted_ticks);

    let mut asked: Vec<(PeerId, Stroke)> = Vec::new();
    for (remote, mut receiver) in inbox.iter_mut() {
        for stroke in receiver.receive() {
            asked.push((remote.0, stroke));
        }
    }

    for (peer, stroke) in asked {
        let refused = stroke.check(&grid).err().or_else(|| {
            let budget = budgets.0.entry(peer).or_insert(SAMPLE_BURST);
            let cost = stroke.cost(&grid) as f32;
            if cost > *budget {
                Some(MapFault::TooMuchGround)
            } else {
                *budget -= cost;
                None
            }
        });

        if let Some(fault) = refused {
            // Answered rather than dropped: a client whose stroke went nowhere has to be able to
            // tell a refusal from a lost packet, and this channel is reliable so that it never has
            // to guess. It rides the map list, which is where every other refusal goes.
            let report = MapList { trouble: Some(fault.to_string()), ..maps.report(None) };
            if let Err(error) =
                sender.send::<_, TerrainChannel>(&report, **server, &NetworkTarget::Single(peer))
            {
                warn!("could not refuse a stroke from {peer:?}: {error}");
            }
            continue;
        }

        let edit = TerrainEdit { stroke, tick: due };
        // Everybody, including the sculptor: their client applies what the server accepted rather
        // than what it asked for, so there is no path where one machine has a stroke the rest do
        // not.
        if let Err(error) = sender.send::<_, TerrainChannel>(&edit, **server, &NetworkTarget::All)
        {
            error!("could not send a stroke on: {error}");
            continue;
        }
        pending.0.push(edit);
        // The map in play now differs from the file it came from, and the menu is the only place
        // that can say so. Nothing here resolves it: saving is explicit, always.
        maps.unsaved = true;
    }
}
