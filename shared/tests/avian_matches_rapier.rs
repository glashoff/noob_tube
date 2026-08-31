//! Does Avian answer the same questions as the rapier `CollisionWorld` it is replacing?
//!
//! **This file is temporary.** It exists only while both engines are in the tree, to hold the
//! migration honest: every query the movement code makes is asked of both and the answers compared.
//! It goes when rapier does.
//!
//! Four things had to be learnt the hard way to get Avian answering at all, and each one fails
//! *silently* — no panic, no warning, just wrong numbers:
//!
//! 1. **`App::finish` must have run.** Avian registers its diagnostics resources in
//!    `Plugin::finish`, which only `App::run` calls. A test that drives schedules itself has to
//!    call it, or the collider tree systems panic on a missing resource — and if the panic is
//!    swallowed, colliders are never sized and every shape behaves as a point at its own centre.
//!    A ray then reports the distance to the centre whatever the shape's size, and a triangle mesh
//!    is missed entirely.
//! 2. **`MoveAndSlide` only sees colliders attached to a rigid body.** Its collider query is
//!    filtered `With<ColliderOf>` and used as the predicate for every cast, so a standalone
//!    `Collider` is invisible to sweeps while remaining visible to `SpatialQuery::cast_ray`. Level
//!    geometry must carry `RigidBody::Static`.
//! 3. **`Collider::cuboid` takes full side lengths**, not half-extents like rapier's.
//! 4. **`Position`, not `Transform`, is where a collider is.** `Transform` is imported into
//!    `Position` by a system, so a collider spawned with only a `Transform` sits at the origin
//!    until that system has run.

use avian3d::prelude::*;
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::movement::{CAPSULE_HALF_HEIGHT, CAPSULE_RADIUS, CAPSULE_Y_OFFSET, SKIN};

const QUAD: [Vec3; 4] = [
    Vec3::new(-50.0, 0.0, -50.0),
    Vec3::new(50.0, 0.0, -50.0),
    Vec3::new(50.0, 0.0, 50.0),
    Vec3::new(-50.0, 0.0, 50.0),
];
const TRIS: [[u32; 3]; 2] = [[0, 1, 2], [0, 2, 3]];
const CRATE: Vec3 = Vec3::new(0.0, 1.0, -8.0);

fn avian_app() -> App {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, PhysicsPlugins::new(FixedPostUpdate)));
    app.world_mut().spawn((
        RigidBody::Static,
        Collider::trimesh(QUAD.to_vec(), TRIS.to_vec()),
        Position::default(),
    ));
    app.world_mut()
        .spawn((RigidBody::Static, Collider::cuboid(2.0, 2.0, 2.0), Position(CRATE)));
    app.finish();
    app.cleanup();
    app.update();
    app
}

fn rapier_world() -> CollisionWorld {
    let mut world = CollisionWorld::new();
    world.add_trimesh(QUAD.to_vec(), TRIS.to_vec());
    world.add_cuboid(CRATE, Vec3::splat(1.0));
    world.rebuild();
    world
}

/// Rays decide what a shot hits and where the ground is. These have to agree exactly — a hitscan
/// that lands differently on the two sides is a shot the player saw hit and the server scored as a
/// miss.
#[test]
fn the_two_engines_agree_about_rays() {
    let mut app = avian_app();
    let rapier = rapier_world();
    let cases = [
        ("straight down onto the floor", Vec3::new(0.0, 5.0, 0.0), Vec3::NEG_Y),
        ("along -Z into the crate", Vec3::new(0.0, 1.0, 0.0), Vec3::NEG_Z),
        ("down onto the crate's top", Vec3::new(0.0, 5.0, -8.0), Vec3::NEG_Y),
        ("past the crate, into nothing", Vec3::new(3.0, 1.0, 0.0), Vec3::NEG_Z),
        (
            "diagonally at the crate",
            Vec3::new(0.0, 3.0, 0.0),
            Vec3::new(0.0, -0.3, -1.0).normalize(),
        ),
    ];
    for (label, origin, direction) in cases {
        let avian = app
            .world_mut()
            .run_system_once(move |query: SpatialQuery| {
                query
                    .cast_ray(origin, Dir3::new(direction).unwrap(), 100.0, true, &default())
                    .map(|hit| hit.distance)
            })
            .unwrap();
        let rapier = rapier.raycast(origin, direction, 100.0);
        match (avian, rapier) {
            (Some(a), Some(r)) => assert!((a - r).abs() < 1e-3, "{label}: avian {a}, rapier {r}"),
            (None, None) => {}
            _ => panic!("{label}: avian {avian:?}, rapier {rapier:?}"),
        }
    }
}

/// The capsule sweep is what the ported Quake-style movement stands on, and the one place the two
/// engines are *not* expected to agree exactly: Avian's move-and-slide runs depenetration passes
/// that hold the capsule a skin width clear of every surface, where ours lets it rest.
///
/// So this asserts the two properties that matter — the horizontal distance travelled, which is
/// what the player feels, and that neither engine lets the capsule through — and records the
/// vertical difference rather than forbidding it.
#[test]
fn the_two_engines_agree_about_capsule_sweeps() {
    let mut app = avian_app();
    let rapier = rapier_world();
    let cases = [
        ("free fall", Vec3::new(0.0, 5.0, 0.0), Vec3::new(0.0, -10.0, 0.0)),
        ("walking on the floor", Vec3::new(0.0, SKIN, 0.0), Vec3::new(0.0, -0.005, -0.086)),
        ("resting exactly on the floor", Vec3::ZERO, Vec3::new(0.0, -0.005, -0.086)),
        ("head-on into the crate", Vec3::new(0.0, 0.0, -6.0), Vec3::new(0.0, 0.0, -1.0)),
        ("clear of the crate's side", Vec3::new(2.5, 0.0, -6.0), Vec3::new(0.0, 0.0, -1.0)),
        ("diagonally into the crate", Vec3::new(0.0, 0.0, -6.0), Vec3::new(1.0, 0.0, -1.0)),
    ];
    for (label, feet, delta) in cases {
        let expected = rapier.sweep_capsule(feet, delta, false);
        let actual = app
            .world_mut()
            .run_system_once(move |slide: MoveAndSlide| {
                let shape = Collider::capsule(CAPSULE_RADIUS, CAPSULE_HALF_HEIGHT * 2.0);
                let start = feet + Vec3::Y * CAPSULE_Y_OFFSET;
                slide
                    .move_and_slide(
                        &shape,
                        start,
                        Quat::IDENTITY,
                        delta,
                        core::time::Duration::from_secs_f32(1.0),
                        &default(),
                        &default(),
                        |_| MoveAndSlideHitResponse::Accept,
                    )
                    .position
                    - start
            })
            .unwrap();

        let horizontal = (actual.xz() - expected.xz()).length();
        assert!(
            horizontal < 0.01,
            "{label}: travelled {actual:?} across the ground, rapier said {expected:?}",
        );
        // Vertically the two differ by design, and not always in Avian's disfavour: pushed
        // diagonally into the crate's corner, rapier squeezes the capsule 7 cm upwards where Avian
        // holds its height. Climbing a box by walking into its edge is a bug we are glad to lose.
        // What must hold either way is that the capsule never ends up inside the floor.
        let feet_end = feet.y + actual.y;
        assert!(feet_end >= -SKIN, "{label}: feet ended at {feet_end}, inside the floor");
    }
}
