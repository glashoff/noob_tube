//! What the level looks like.
//!
//! Where it *is* lives in `noob_tube_shared::level`, so the server collides against the same
//! numbers. This module only turns them into meshes.

use bevy::prelude::*;
use noob_tube_shared::level::{self, CRATES, CRATE_HALF_EXTENT, RAMP_HALF_EXTENTS};
use noob_tube_shared::terrain::{self, Terrain};
use noob_tube_shared::types::Authored;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<LevelRoot>()
            .add_systems(Startup, (spawn_ground, level::spawn_level));
    }
}

/// How many cells of height field one drawn tile covers.
///
/// 64, which is the number §6 of the plan tiles editing on: one stroke should dirty one tile and
/// rebuild that tile alone.
const MESH_TILE: u32 = 64;

/// How many samples the drawn ground skips between vertices.
///
/// The collider keeps every sample; only the picture is coarse. That is an ordinary level of
/// detail and not the mismatch this replaced — a plane the size of the terrain was a *different
/// shape*, this is the same shape at fewer points, and the difference on a hill of hundred-metre
/// radius is centimetres.
///
/// It is here because it had to be. At one vertex per sample the ground is half a million
/// triangles, and standing on it they are each about two pixels: a GPU shades in 2x2 quads, so
/// sub-pixel triangles cost four times what they cover. Measured on the integrated GPU this game
/// asks for, drawing the ground at all took 62 frames a second down to 22, and tiling it into 64
/// pieces changed nothing — from ground level you can see most of a 512 m map, so there is nothing
/// to cull.
///
/// Two rather than four, because that is where the win is: 22 fps at every sample, 45 at every
/// second, 47 at every fourth. Four times fewer triangles buys another four per cent, and the rest
/// of the gap to an empty screen is the ground covering it, which no amount of thinning removes.
const MESH_STRIDE: u32 = 2;

/// One tile of the height field as something to look at.
///
/// A plain mesh with the level's own material, which is the smallest thing that answers "is that
/// hill where I think it is". Proper terrain drawing — layers chosen by slope and height, triplanar
/// projection, tile break — is its own step; none of it changes the geometry this builds.
///
/// Built from the same [`Terrain`] the collider is, so the two cannot be built from different maps.
/// They are not quite the same *shape*, and [`MESH_STRIDE`] is why: the picture skips samples the
/// collider keeps, which on the sharpest lip of a ravine puts the drawn ground 62 cm from the
/// ground underfoot — see `the_drawn_ground_stays_near_the_ground_underfoot`. When the server
/// starts sending the field, both come from the copy it sent, and this call site is the only thing
/// that changes.
///
/// Normals are read from the *whole* field rather than from the tile, which is what stops a seam
/// showing: two tiles meeting along an edge share those vertices' positions, and they have to agree
/// about which way the ground faces there as well.
fn ground_mesh(terrain: &Terrain, ix0: u32, iz0: u32, cells_x: u32, cells_z: u32) -> Mesh {
    let grid = terrain.grid;
    let step = MESH_STRIDE.max(1);
    let (across, down) = (cells_x / step, cells_z / step);
    let (wide, deep) = ((across + 1) as usize, (down + 1) as usize);
    let mut positions = Vec::with_capacity(wide * deep);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(wide * deep);
    let mut uvs = Vec::with_capacity(wide * deep);

    for row in 0..=down {
        for column in 0..=across {
            let (ix, iz) = (ix0 + column * step, iz0 + row * step);
            let here = grid.world_of(ix, iz);
            positions.push([here.x, terrain.height_at(ix, iz), here.y]);
            // Central differences, one sample either side, falling back to this sample at the rim
            // of the map. The cross product of the two tangents comes out as this without building
            // them.
            let west = terrain.height_at(ix.saturating_sub(step), iz);
            let east = terrain.height_at((ix + step).min(grid.nx - 1), iz);
            let south = terrain.height_at(ix, iz.saturating_sub(step));
            let north = terrain.height_at(ix, (iz + step).min(grid.nz - 1));
            let normal = Vec3::new(west - east, 2.0 * step as f32 * grid.spacing, south - north);
            normals.push(normal.normalize().into());
            uvs.push([here.x, here.y]);
        }
    }

    // Two triangles a cell, wound so the front face is the one you stand on. Getting this the other
    // way round leaves ground that is solid, lit from underneath and invisible from above.
    let mut indices = Vec::with_capacity(across as usize * down as usize * 6);
    for iz in 0..down as usize {
        for ix in 0..across as usize {
            let here = (iz * wide + ix) as u32;
            let next_row = here + wide as u32;
            indices.extend_from_slice(&[here, next_row, here + 1]);
            indices.extend_from_slice(&[here + 1, next_row, next_row + 1]);
        }
    }

    Mesh::new(
        bevy::mesh::PrimitiveTopology::TriangleList,
        bevy::asset::RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(bevy::mesh::Indices::U32(indices))
}

/// Root of everything the level owns.
///
/// Giving the level a root is worth more than the tidier inspector tree it produces: despawning it
/// takes the whole level with it, which is what a map change needs.
///
/// One rule comes with it. Child transforms are relative to this entity, so as long as it stays at
/// the identity, world and local coordinates agree — and they have to, because the collision
/// geometry the `Level` queries is in world space and knows nothing about the hierarchy. Moving this
/// entity would slide the visible level off its collision.
#[derive(Component, Reflect)]
#[reflect(Component)]
pub struct LevelRoot;

/// Builds what the level looks like. What it collides as is
/// [`level::spawn_level`](noob_tube_shared::level::spawn_level), spawned alongside this.
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
            LevelRoot,
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

    // The height field itself, not a plane the size of it. A hill you can walk into and not see is
    // worse than no hill, and the collider is that shape either way.
    //
    // In tiles, and that is not tidiness. As one mesh the ground is half a million triangles with
    // no way to leave any of them out, and it costs the whole map whichever way you are facing:
    // measured, 51 frames a second became 20. A tile has an AABB, so the ones behind you are culled
    // before they reach the GPU. It is also the unit §6 wants for editing — a stroke dirties a tile
    // and that tile alone rebuilds.
    let terrain = terrain::default_terrain();
    let ground = materials.add(StandardMaterial {
        base_color: Color::srgb(0.30, 0.33, 0.30),
        perceptual_roughness: 0.95,
        ..default()
    });
    let cells_x = terrain.grid.nx - 1;
    let cells_z = terrain.grid.nz - 1;
    for iz in (0..cells_z).step_by(MESH_TILE as usize) {
        for ix in (0..cells_x).step_by(MESH_TILE as usize) {
            let wide = MESH_TILE.min(cells_x - ix);
            let deep = MESH_TILE.min(cells_z - iz);
            commands.spawn((
                Name::from(format!("Ground {ix},{iz}")),
                Authored,
                Mesh3d(meshes.add(ground_mesh(&terrain, ix, iz, wide, deep))),
                MeshMaterial3d(ground.clone()),
                Transform::IDENTITY,
                ChildOf(level),
            ));
        }
    }

    // The ramp. Its pose is derived rather than written down, so that the visible slope and the
    // one a vehicle drives up are the same slope — see `level::ramp_pose`.
    let (ramp_at, ramp_facing) = level::ramp_pose();
    commands.spawn((
        Name::from("Ramp"),
        Authored,
        Mesh3d(meshes.add(Cuboid::from_size(RAMP_HALF_EXTENTS * 2.0))),
        MeshMaterial3d(materials.add(Color::srgb(0.42, 0.40, 0.38))),
        Transform::from_translation(ramp_at).with_rotation(ramp_facing),
        ChildOf(level),
    ));

    // A few boxes to bump into. The positions come from `shared`, which is also what the collision
    // world is built from, so the visible crate and the one you collide with cannot drift apart.
    let box_mesh = meshes.add(Cuboid::new(
        CRATE_HALF_EXTENT * 2.0,
        CRATE_HALF_EXTENT * 2.0,
        CRATE_HALF_EXTENT * 2.0,
    ));
    let box_material = materials.add(Color::srgb(0.55, 0.4, 0.3));
    for (i, centre) in CRATES.into_iter().enumerate() {
        commands.spawn((
            Name::from(format!("Crate {i}")),
            Authored,
            Mesh3d(box_mesh.clone()),
            MeshMaterial3d(box_material.clone()),
            Transform::from_translation(centre),
            ChildOf(props),
        ));
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// What drawing every second sample costs, in metres.
    ///
    /// The collider keeps every sample and the picture does not, so the surface you see and the
    /// surface you stand on differ wherever the ground bends faster than the drawn triangles can
    /// follow. That is a fair trade for twice the frame rate as long as the number is known, and
    /// this is where it is known: **62 cm at its worst**, on the lips of the ravines, and
    /// centimetres over the hills — the whole map is within 0.7 m.
    ///
    /// It matters more than it did, because there is now a slope limit: a player stopped by ground
    /// they cannot see is worse than one stopped by ground they can. The fix when it comes is level
    /// of detail by distance — full resolution under the camera, coarse at the horizon — not a
    /// finer mesh everywhere, which is what cost the frame rate in the first place.
    #[test]
    fn the_drawn_ground_stays_near_the_ground_underfoot() {
        let terrain = terrain::default_terrain();
        let grid = terrain.grid;
        let step = MESH_STRIDE.max(1);
        let mut worst: f32 = 0.0;
        for iz in 0..grid.nz {
            for ix in 0..grid.nx {
                let (x0, z0) = (ix / step * step, iz / step * step);
                let (x1, z1) = (x0 + step, z0 + step);
                if x1 >= grid.nx || z1 >= grid.nz {
                    continue;
                }
                let (u, v) =
                    ((ix - x0) as f32 / step as f32, (iz - z0) as f32 / step as f32);
                let (h00, h10) = (terrain.height_at(x0, z0), terrain.height_at(x1, z0));
                let (h01, h11) = (terrain.height_at(x0, z1), terrain.height_at(x1, z1));
                // Each cell is two triangles split along the anti-diagonal `u + v = 1`, which is
                // the edge `[here + 1, next_row]` above.
                let drawn = if u + v <= 1.0 {
                    h00 + (h10 - h00) * u + (h01 - h00) * v
                } else {
                    h11 + (h01 - h11) * (1.0 - u) + (h10 - h11) * (1.0 - v)
                };
                worst = worst.max((drawn - terrain.height_at(ix, iz)).abs());
            }
        }
        assert!(worst < 0.7, "the drawn ground is {worst:.2} m from the ground underfoot");
    }
}
