//! Gizmo overlay for the collision shapes the movement step reasons about.
//!
//! None of this geometry exists as an entity. The capsule is a shape handed to a query, and the
//! ground probe is a ray cast and discarded, so there is nothing to look at while the game runs —
//! which is how the M1 freeze stayed hidden: the capsule had settled 1.6 mm into the floor, visible
//! in the numbers only after an afternoon of comparing logs.
//!
//! Gizmos are immediate-mode: each call draws for one frame and leaves nothing behind. That is why
//! this can run every frame without spawning or despawning anything.

use bevy::prelude::*;
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::movement::{
    CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET, CROUCH_CAPSULE_HALF_HEIGHT,
    CROUCH_CAPSULE_Y_OFFSET, GROUND_SNAP_DIST,
};

use crate::local_player::LocalPlayer;

/// Green while the player is grounded, red while airborne. This one bool decides whether the
/// player accelerates to MAX_SPEED or the much lower MAX_SPEED_AIR, and it flickering on flat
/// ground is exactly what braked the player to 3 m/s before the raycast replaced the shape cast.
const GROUNDED: Color = Color::srgb(0.2, 0.9, 0.3);
const AIRBORNE: Color = Color::srgb(0.9, 0.25, 0.2);
const PROBE: Color = Color::srgb(0.35, 0.6, 1.0);
const CONTACT: Color = Color::srgb(1.0, 0.85, 0.2);

pub struct DebugDrawPlugin;

impl Plugin for DebugDrawPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            draw_collision.run_if(bevy::input::common_conditions::input_toggle_active(
                false,
                KeyCode::F3,
            )),
        );
    }
}

/// Update: draws the capsule, the ground probe and the ground contact.
///
/// Drawn from the player's own state rather than from the transform, so what appears is what the
/// movement step actually used, not what the camera was placed at afterwards.
///
/// Its usefulness in first person is limited, and worth knowing before trusting it. The capsule
/// surrounds the camera at 0.35 m, so at a 90 degree field of view it fills the screen and reads as
/// stray lines. The probe is vertical, so looking straight down at it projects it to a point. Only
/// the contact cross is informative from inside, which is enough for the failure it was written
/// for — a gap between the cross and the capsule's base is the capsule sinking into the floor.
///
/// It comes into its own in M3, when other players are drawn and can be looked at from outside.
fn draw_collision(
    player: Single<&LocalPlayer>,
    world: Option<Res<CollisionWorld>>,
    mut gizmos: Gizmos,
) {
    let Some(world) = world else { return };
    let state = &player.state;

    let (half_height, y_offset) = if state.crouching {
        (CROUCH_CAPSULE_HALF_HEIGHT, CROUCH_CAPSULE_Y_OFFSET)
    } else {
        (CAPSULE_HALF_HEIGHT, CAPSULE_Y_OFFSET)
    };
    let colour = if state.on_ground { GROUNDED } else { AIRBORNE };

    gizmos.primitive_3d(
        &Capsule3d::new(CAPSULE_RADIUS, half_height * 2.0),
        Isometry3d::from_translation(state.position + Vec3::Y * y_offset),
        colour,
    );

    // The probe `is_grounded` casts. Its length is what decides "standing" versus "falling".
    let probe_end = state.position - Vec3::Y * GROUND_SNAP_DIST;
    gizmos.line(state.position, probe_end, PROBE);

    // Where the ground actually is. A gap between this and the capsule's base is the sinking that
    // made shape casts report a contact at zero distance every tick.
    if let Some(ground) = world.ground_height_below(state.position) {
        let hit = Vec3::new(state.position.x, ground, state.position.z);
        gizmos.cross(Isometry3d::from_translation(hit), 0.3, CONTACT);
    }
}
