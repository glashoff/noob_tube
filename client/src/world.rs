//! The level. For M1 that is one very large flat plane.

use bevy::prelude::*;
use noob_tube_shared::collision::CollisionWorld;
use noob_tube_shared::types::Authored;

/// Half-extent of the ground plane, in metres.
const HALF_EXTENT: f32 = 250.0;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<Level>().add_systems(Startup, spawn_ground);
    }
}

/// Root of everything the level owns.
///
/// Giving the level a root is worth more than the tidier inspector tree it produces: despawning it
/// takes the whole level with it, which is what a map change needs.
///
/// One rule comes with it. Child transforms are relative to this entity, so as long as it stays at
/// the identity, world and local coordinates agree — and they have to, because the collision
/// geometry in `CollisionWorld` is in world space and knows nothing about the hierarchy. Moving this
/// entity would slide the visible level off its collision.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct Level;

/// Spawns the ground twice: once as a render mesh, once as collision geometry.
///
/// Keeping the two separate is deliberate — real levels use a simplified collision mesh, and
/// building that split in now means no rework when actual geometry arrives.
fn spawn_ground(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // Parents need a Transform and a Visibility of their own: both propagate down the tree, and a
    // child under a parent that has neither never becomes visible.
    let level = commands
        .spawn((
            Name::from("Level"),
            Level,
            Authored,
            Transform::IDENTITY,
            Visibility::default(),
        ))
        .id();
    let props = commands
        .spawn((
            Name::from("Props"),
            Authored,
            Transform::IDENTITY,
            Visibility::default(),
            ChildOf(level),
        ))
        .id();

    commands.spawn((
        Name::from("Ground"),
        Authored,
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
        ChildOf(level),
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
            Authored,
            Mesh3d(box_mesh.clone()),
            MeshMaterial3d(box_material.clone()),
            Transform::from_translation(offset),
            ChildOf(props),
        ));
        // `offset` is a world position here, which only holds while the level sits at the identity.
        world.add_cuboid(offset, Vec3::splat(1.0));
    }

    world.rebuild();
    commands.insert_resource(world);

    commands.spawn((
        Name::from("Sun"),
        Authored,
        DirectionalLight {
            illuminance: 10_000.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(50.0, 100.0, 50.0).looking_at(Vec3::ZERO, Vec3::Y),
        ChildOf(level),
    ));
}
