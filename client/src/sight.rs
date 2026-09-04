//! How far a player can see, and the haze that ends it.
//!
//! **The fog is not a picture of distance, it is what makes cutting the distance invisible.** A
//! haze on its own buys nothing: a fragment tinted to the colour of the sky costs exactly what a
//! fragment costs, and the ground past it is still shaded, still sampled, still there. What is
//! worth having is the camera's far plane, which culls — and a far plane on its own is a wall,
//! ground ending in mid-air against a sky it does not reach. Each fixes what the other breaks, so
//! this module never has one without the other and both come off one number.
//!
//! **The two colours are one colour.** The haze fades to [`ClearColor`] rather than to a colour of
//! its own, and that is what makes the cut invisible rather than merely cheap: at the far plane the
//! fog is already fully opaque, so a fragment clipped away and a fragment drawn are the same pixel.
//! Written as a second constant it would be right on the day it was typed and wrong the day
//! somebody changed the sky.
//!
//! **Nothing here is map content and nothing here travels.** A short horizon shows a player less
//! and never more — off is the whole world — so this cannot be turned into an advantage, only into
//! frames. It is the same rule [`grass`](crate::grass) lives under: this decides a picture, and a
//! picture has never been what anybody may stand on or be shot through.
//!
//! No shader knows about any of this. Bevy applies [`DistanceFog`] inside
//! `main_pass_post_lighting_processing`, which `ground.wgsl` and `water.wgsl` already call as their
//! last line and which the grass gets from `StandardMaterial` without asking.

use bevy::pbr::{DistanceFog, FogFalloff};
use bevy::prelude::*;

use crate::settings::Settings;

/// Where the haze begins, as a fraction of where it ends.
///
/// The whole of the visible range cannot be haze — ground that starts fading at the player's feet
/// reads as weather rather than as distance — and neither can a thin band at the end, which is a
/// visible curtain to walk towards. The predecessor settles at 25 m into a 60 m fog; this is that
/// ratio rounded to a number worth writing down.
const HAZE_FROM: f32 = 0.4;

/// The camera, and the two things on it that a horizon is made of.
///
/// A named type because the tuple is three deep and reads as noise inline; `Option` around it
/// because there is a frame or two before [`spawn_player`](crate::local_player) has run, and no
/// camera is a reason to do nothing rather than a reason to complain.
type Eye<'w, 's> =
    Single<'w, 's, (Entity, &'static mut Projection, Option<&'static mut DistanceFog>), With<Camera3d>>;

pub struct SightPlugin;

impl Plugin for SightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, keep_the_horizon);
    }
}

/// Update: keeps the fog and the far plane saying the same thing the slider does.
///
/// Every frame rather than on a change, and it costs a `Single` lookup and two comparisons to say
/// "nothing to do". The alternative is change detection on [`Settings`], which is right until
/// something else touches the camera — a respawn, a map switch, an inspector — and then it is a
/// horizon that silently stops matching its own setting. This cannot get out of step because it
/// never remembers anything.
///
/// **Read before writing.** A `Mut` marks its component changed the moment it is dereferenced
/// mutably, whether or not anything is written through it, and a `Projection` marked changed every
/// frame is a frustum recomputed every frame — the exact cost this module exists to save.
fn keep_the_horizon(
    mut commands: Commands,
    settings: Res<Settings>,
    sky: Res<ClearColor>,
    eye: Option<Eye>,
) {
    let Some(eye) = eye else {
        return;
    };
    let (entity, mut projection, fog) = eye.into_inner();

    // The far plane is the setting, and off is whatever the projection was built with — not a large
    // number of this module's choosing, which would crop a map bigger than the one it was picked
    // for. See `Settings::sight`, where that distinction is the whole of what the method is for.
    let reach = settings.sight();
    let far = reach.unwrap_or_else(|| PerspectiveProjection::default().far);
    let stale = match &*projection {
        Projection::Perspective(lens) => lens.far != far,
        _ => false,
    };
    // `&&` short-circuits, which is the whole reason the test is a separate `stale` above: the
    // mutable deref that marks the projection changed is never reached on a frame that has nothing
    // to write.
    if stale && let Projection::Perspective(lens) = &mut *projection {
        lens.far = far;
    }

    let Some(reach) = reach else {
        // Removed rather than pushed out to a distance nothing reaches. A fog component is a uniform
        // and a branch in every fragment that ends in `main_pass_post_lighting_processing`, and off
        // should cost what off costs.
        if fog.is_some() {
            commands.entity(entity).remove::<DistanceFog>();
        }
        return;
    };

    let (start, end) = (reach * HAZE_FROM, reach);
    // `DistanceFog` has no `PartialEq` to compare against, and the two fields that can differ are
    // the two this writes.
    let fits = fog.as_deref().is_some_and(|had| {
        had.color == sky.0
            && matches!(had.falloff, FogFalloff::Linear { start: had_start, end: had_end }
                if had_start == start && had_end == end)
    });
    if fits {
        return;
    }

    // Linear rather than exponential, because a slider wants a promise it can keep: linear fog is
    // fully opaque *at* `end` and nowhere short of it, which is what lets the far plane sit exactly
    // there. An exponential falloff only approaches opacity, so the plane would have to stand
    // somewhere past a distance that no longer means anything the player can read off the dialog.
    let wanted = DistanceFog {
        color: sky.0,
        falloff: FogFalloff::Linear { start, end },
        ..default()
    };
    match fog {
        Some(mut had) => *had = wanted,
        None => {
            commands.entity(entity).insert(wanted);
        }
    }
}
