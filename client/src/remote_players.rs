//! Draws the players the server replicates to us.
//!
//! This is the receiving half of replication. Nothing here spawns a player: the entities arrive
//! from the server carrying [`Player`], [`PlayerState`] and [`Aim`], and this module places them,
//! claims our own, and sends our input. What they are *drawn* as lives in [`crate::character`].
//!
//! The values are already smoothed by the time anything here reads them. The server marks every
//! player `Interpolated` for every client but its owner, and lightyear then keeps a history of
//! received updates and writes a blend of two of them back into the component each frame. That
//! happens in place, on the same entity — there is no second copy to look up.

use bevy::prelude::*;
use lightyear::prelude::*;
use lightyear::prelude::input::native::{ActionState, InputMarker};
use noob_tube_shared::player::{Aim, Player, PlayerInput, PlayerState};
use noob_tube_shared::vehicle::Driving;
use noob_tube_shared::tuning::NetConfig;

use crate::local_player::CurrentInput;

pub struct RemotePlayersPlugin;

impl Plugin for RemotePlayersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (claim_own_player, hide_the_seated, place_bodies)
                .chain()
                // Interpolation writes the smoothed values into `PlayerState` and `Aim` in Update
                // as well. Without this the bodies would render whatever last frame's sample
                // was — one frame of lag added on top of the delay interpolation already costs.
                .after(InterpolationSystems::All),
        )
            // Inputs must be written before lightyear packs them for sending, which is what this
            // system set marks.
            .add_systems(Update, report_input_delay)
            .add_systems(
                FixedPreUpdate,
                send_input
                    .in_set(client::input::InputSystems::WriteClientInputs)
                    // A rollback re-runs this schedule for each replayed tick, and the input for
                    // those ticks has to come from the buffer, not from whatever key is held now.
                    .run_if(not(resource_exists::<Rollback>)),
            );
    }
}

/// Update: reports the input delay lightyear settled on, once, when the clocks agree.
///
/// The configured value is a floor and a ceiling, not the answer: between them lightyear picks a
/// delay from the measured round trip, so what is actually in effect is only knowable at runtime.
/// Printing it also closes the gap between "the setting was read" and "the setting is doing
/// something", which is not the same thing and has twice not been today.
fn report_input_delay(
    timeline: Res<client::LocalTimelineSync>,
    net: Res<NetConfig>,
    mut reported: Local<bool>,
) {
    if *reported || !timeline.is_synced() {
        return;
    }
    *reported = true;
    let ticks = timeline.input_delay();
    info!(
        "clocks synced: input delay {ticks} ticks ({:?})",
        net.tick_duration() * u32::from(ticks)
    );
}

/// Update: follows the replicated state with the body.
///
/// The transform is the player's *feet*, which is what `PlayerState::position` is and what a
/// character model measures from — the offset the placeholder capsule needed is gone with it.
///
/// Yaw only. Pitch turns the head on a real body, not the whole person, and the head is a bone
/// inside an animated skeleton now rather than a box on a stick: aiming it is its own job and it is
/// not done yet. Standing still and looking up, a player's body no longer leans back — which is
/// right — but neither does anything of theirs move, which is the part still missing.
///
/// Our own player is excluded for the same reason it gets no body: the camera is inside it.
fn place_bodies(
    mut bodies: Query<(&PlayerState, &Aim, &mut Transform), (With<client::Remote>, Without<Predicted>)>,
) {
    for (state, aim, mut transform) in bodies.iter_mut() {
        transform.translation = state.position;
        transform.rotation = Quat::from_rotation_y(aim.yaw);
    }
}

/// Update: a player in a vehicle is inside the bodywork, so stop drawing them.
///
/// Hidden rather than despawned. They are still a replicated entity with a pose and a hitbox — they
/// can still be shot, and they come back the moment they get out — and rebuilding the meshes on
/// every exit would be work done for nothing.
///
/// Cheap enough to run every frame: it writes only when the answer changes, so the change detection
/// downstream stays quiet.
fn hide_the_seated(mut bodies: Query<(&mut Visibility, Has<Driving>), With<PlayerState>>) {
    for (mut visibility, driving) in bodies.iter_mut() {
        let wanted = if driving { Visibility::Hidden } else { Visibility::Inherited };
        if *visibility != wanted {
            *visibility = wanted;
        }
    }
}

/// Update: takes ownership of the player the server says is ours.
///
/// `Controlled` arrives on exactly one replicated entity: the one the server marked `ControlledBy`
/// our connection. That is a better answer than comparing peer ids, because the server decides it
/// and the client cannot get it wrong.
///
/// `InputMarker` tells lightyear which `ActionState` this client fills in, as opposed to the ones it
/// merely receives for other players.
///
/// It used to copy the server's spawn position into a separate local simulation as well. There is
/// no separate simulation any more — this entity is the simulation — so it starts wherever the
/// server put it, and there is nothing to adopt.
fn claim_own_player(
    mine: Query<(Entity, &Player), (With<client::Remote>, Added<Controlled>)>,
    mut commands: Commands,
) {
    for (entity, player) in mine.iter() {
        commands
            .entity(entity)
            .insert(InputMarker::<PlayerInput>::default());
        info!("player {} is ours", player.peer);
    }
}

/// FixedPreUpdate: hands this tick's input to lightyear.
///
/// Writing it into `ActionState` is the whole of sending: the plugin buffers it, packs the last N
/// ticks into the next packet, and keeps the history that M4's rollback will replay from.
///
/// This is also the input the local prediction steps on, in the same tick: the client does not wait
/// for the server to tell it what its own input did.
fn send_input(
    input: Res<CurrentInput>,
    mut action: Single<&mut ActionState<PlayerInput>, With<InputMarker<PlayerInput>>>,
) {
    action.0 = input.0;
}
