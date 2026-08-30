//! The level. For M1 that is one very large flat plane.

use bevy::prelude::*;
use noob_tube_shared::collision::CollisionWorld;

/// Half-extent of the ground plane, in metres.
const HALF_EXTENT: f32 = 250.0;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_ground);
    }
}

/// Spawns the ground twice: once as a render mesh, once as collision geometry.
///
/// Keeping the two separate is deliberate — real levels use a simplified collision mesh, and
/// building that split in now means no rework when actual geometry arrives.
fn spawn_ground(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Name::from("Ground"),
        Mesh3d(meshes.add(
            Plane3d::default()
                .mesh()
                .size(HALF_EXTENT * 2.0, HALF_EXTENT * 2.0),
        )),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.30, 0.33, 0.30),
            perceptual_roughness: 0.95,
            ..default()
        })),
        Transform::IDENTITY,
    ));

    let mut world = CollisionWorld::new();
    world.add_trimesh(
        vec![
            Vec3::new(-HALF_EXTENT, 0.0, -HALF_EXTENT),
            Vec3::new(HALF_EXTENT, 0.0, -HALF_EXTENT),
            Vec3::new(HALF_EXTENT, 0.0, HALF_EXTENT),
            Vec3::new(-HALF_EXTENT, 0.0, HALF_EXTENT),
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    // A few boxes to bump into, so collision and sliding are visible at all.
    let box_mesh = meshes.add(Cuboid::new(2.0, 2.0, 2.0));
    let box_material = materials.add(Color::srgb(0.55, 0.4, 0.3));
    for (i, offset) in [
        Vec3::new(6.0, 1.0, -8.0),
        Vec3::new(-5.0, 1.0, -12.0),
        Vec3::new(0.0, 1.0, -18.0),
    ]
    .into_iter()
    .enumerate()
    {
        commands.spawn((
            Name::from(format!("Crate {i}")),
            Mesh3d(box_mesh.clone()),
            MeshMaterial3d(box_material.clone()),
            Transform::from_translation(offset),
        ));
        world.add_cuboid(offset, Vec3::splat(1.0));
    }

    world.rebuild();
    commands.insert_resource(world);

    commands.spawn((
        Name::from("Sun"),
        DirectionalLight {
            illuminance: 10_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(50.0, 100.0, 50.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));
}
