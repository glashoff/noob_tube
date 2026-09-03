//! The sea: the surface a map's water level is drawn as, and the material that makes it read as
//! water.
//!
//! **Everything interesting about water comes from its depth**, and this client already knows the
//! depth exactly: it holds the height field, so the distance from the surface down to the ground is
//! a subtraction rather than a depth pre-pass, a scene depth texture and a reconstruction from the
//! projection matrix. It is measured once per vertex when the mesh is built and interpolated across
//! the triangle, and out of that one number come the colour from clear shallows to dark deeps, the
//! opacity, the foam at the shoreline — and the shoreline itself, which lands exactly where the
//! ground crosses the water level rather than on the nearest grid cell.
//!
//! The look is the one the predecessor project settled on (`../webgame`, `client/src/waterMaterial.ts`),
//! ported rather than reinvented: its numbers are the result of somebody looking at a lake for a
//! week, and there is nothing about this engine that would make different ones better.
//!
//! What is deliberately *not* here yet: swimming, buoyancy, and anything else that would make water
//! part of the simulation. Until one of those exists the surface is a picture, which is what lets
//! the water level travel without a commit tick — see
//! [`WaterLevel`](noob_tube_shared::terrain::WaterLevel).

use bevy::asset::uuid_handle;
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use noob_tube_shared::terrain::{Ground, Terrain};
use noob_tube_shared::types::Authored;

/// The shader, by the path the asset server knows it under.
const SHADER: &str = "shaders/water.wgsl";

/// What the water is drawn with: Bevy's PBR, with the depth-driven surface bolted on to its
/// fragment stage. The base material is what keeps the sun, the shadows and the tone mapping.
pub type WaterMaterial = ExtendedMaterial<StandardMaterial, WaterSurface>;

/// The one material every water surface wears. Its whole look is a constant, and what varies from
/// map to map is the mesh.
pub const WATER_MATERIAL: Handle<WaterMaterial> =
    uuid_handle!("b4c1a7e2-0d95-4f38-9c62-3a71e5d80b46");

pub struct WaterPlugin;

impl Plugin for WaterPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<WaterMaterial>::default())
            .add_systems(Startup, mix_the_water)
            // Beside the ground and on the same condition: the depth under every vertex is a
            // subtraction against the height field, so a stroke that moves the ground moves the
            // water's depth with it, and a map that arrives brings both.
            .add_systems(Update, dress_the_water.run_if(resource_exists_and_changed::<Ground>));
    }
}

/// How many quads across the water surface may be, at most.
///
/// The surface is flat, so its geometry has nothing to resolve but the *depth* — not the shape of
/// anything, and not the shoreline either, which the fragment shader cuts on the interpolated
/// depth. At one quad per sample a flooded 513² map is 263k vertices for a sheet of water; at 128
/// it is 17k, and the difference is invisible because there is nothing between the samples for the
/// extra vertices to have found.
pub const MAX_WATER_AXIS: u32 = 128;

/// The look, laid out the way a uniform wants it.
///
/// In Rust rather than as constants in the shader for one reason that matters: **these are linear
/// colours**. They were picked as sRGB hex — the form anybody reads a colour in — and a shader is
/// the wrong place to convert, because the conversion would then happen per pixel and be written
/// out by hand. Bevy's own `LinearRgba` does it here, once, at startup.
#[derive(Clone, Copy, Default, ShaderType, Debug, Reflect)]
pub struct WaterLook {
    /// `rgb` the colour of shallow water. `w` metres of depth over which the colour and the opacity
    /// reach their deep-end values.
    shallow: Vec4,
    /// `rgb` the colour of deep water. `w` the width of the foam band at the shore, in metres of
    /// depth.
    deep: Vec4,
    /// `rgb` the colour of foam. `w` metres per wave — larger is a longer, calmer swell.
    foam: Vec4,
    /// `x` opacity at the shore, `y` in the deep, `z` how fast the waves travel, `w` how far the
    /// wave tilts the surface normal.
    numbers: Vec4,
}

impl Default for WaterSurface {
    fn default() -> Self {
        let linear = |colour: Srgba, w: f32| {
            let rgb = LinearRgba::from(colour);
            Vec4::new(rgb.red, rgb.green, rgb.blue, w)
        };
        Self {
            look: WaterLook {
                shallow: linear(Srgba::hex("2f7f7a").unwrap(), 3.5),
                deep: linear(Srgba::hex("0a2b46").unwrap(), 0.55),
                foam: linear(Srgba::hex("dff2f4").unwrap(), 2.4),
                // The last of these is small because the wave gradient carries the wave *number* —
                // up to some 4 rad/m on the finest octave — so a little of it goes a long way. It
                // is a tilt in the surface normal, not a height.
                numbers: Vec4::new(0.22, 0.93, 0.7, 0.055),
            },
        }
    }
}

/// The extension: one uniform and one fragment shader. No textures at all — every wave in it is
/// arithmetic, which is what lets the surface be built and rebuilt from nothing but the height
/// field.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct WaterSurface {
    // 100 and up is the range Bevy leaves free for an extension; the base material owns 0..99.
    #[uniform(100)]
    pub look: WaterLook,
}

impl MaterialExtension for WaterSurface {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }
}

/// Startup: writes the one material every water surface wears.
///
/// Once, rather than on each map: the look is a constant and the mesh is what carries everything
/// that differs. Written under a fixed handle so [`dress_the_water`] can point at it without
/// keeping a resource of its own.
fn mix_the_water(mut materials: ResMut<Assets<WaterMaterial>>) {
    let written = materials.insert(
        &WATER_MATERIAL,
        ExtendedMaterial {
            base: StandardMaterial {
                // White, because the extension has already decided what colour this pixel is and
                // the base tint multiplies into it.
                base_color: Color::WHITE,
                // Water must be drawn after everything behind it and must not write depth, or the
                // far side of the same lake disappears behind the near side. `Blend` is what puts
                // it in the transparent phase, where both of those are true.
                alpha_mode: AlphaMode::Blend,
                // Seen from below as well: a player standing in a valley that fills up is under the
                // surface, and a single-sided sheet would simply vanish.
                double_sided: true,
                cull_mode: None,
                perceptual_roughness: 0.05,
                metallic: 0.0,
                ..default()
            },
            extension: WaterSurface::default(),
        },
    );
    if let Err(error) = written {
        error!("the water's material could not be written: {error}");
    }
}

/// One drawn sea, so the next map can take the last one's down.
#[derive(Component)]
struct WaterSurfaceMesh;

/// Update: draws the map's water, and takes down what was there before.
///
/// Rebuilt whole rather than in tiles, which is the opposite of what the ground does and right for
/// the same reason the ground is tiled: this is 17k vertices at most for the entire map, most maps
/// have far less of it than that, and a sheet of water has no silhouette to cull against. What it
/// costs is one rebuild per change to the ground — which is exactly when it is wrong, since every
/// vertex of it measures the ground underneath.
fn dress_the_water(
    ground: Res<Ground>,
    root: Single<Entity, With<crate::world::LevelRoot>>,
    old: Query<Entity, With<WaterSurfaceMesh>>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    for surface in old.iter() {
        commands.entity(surface).despawn();
    }
    let Some(mesh) = water_mesh(&ground.0) else {
        return;
    };
    commands.spawn((
        Name::from("Water"),
        Authored,
        WaterSurfaceMesh,
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(WATER_MATERIAL),
        Transform::IDENTITY,
        ChildOf(*root),
    ));
}

/// The samples one axis of the water grid stands on: every `step`-th, and always the last.
///
/// Always the last is what keeps the sheet reaching the edge of the map. A step that does not
/// divide the axis evenly would otherwise stop short of it by up to a step, and the map would have
/// a strip of dry ground along two of its sides that no ground anywhere holds up.
fn axis(n: u32) -> Vec<u32> {
    let step = ((n - 1).div_ceil(MAX_WATER_AXIS)).max(1);
    let mut out: Vec<u32> = (0..n - 1).step_by(step as usize).collect();
    out.push(n - 1);
    out
}

/// The water surface of a map: a flat sheet at its water level, carrying the depth under each
/// vertex. `None` when the map is dry, and equally when nothing on it is submerged.
///
/// **Quads are emitted only where at least one corner is under water**, so a pond in one valley
/// costs a pond rather than a map-sized plane. A quad that straddles the waterline is kept whole
/// and the shader fades its dry half out — that is what puts the shore on the waterline instead of
/// on the nearest grid cell.
fn water_mesh(terrain: &Terrain) -> Option<Mesh> {
    let level = terrain.water_y?;
    let grid = terrain.grid;
    let (xs, zs) = (axis(grid.nx), axis(grid.nz));

    // The depth at every node of the reduced grid, once: each quad reads its four corners from
    // here rather than measuring the ground four times over.
    let mut depth = vec![0.0f32; xs.len() * zs.len()];
    let mut wet = false;
    for (b, iz) in zs.iter().enumerate() {
        for (a, ix) in xs.iter().enumerate() {
            // `surface_at`, which is the ground with its roughening in it — the same number the
            // collider stands on. Taking the authored height instead would leave the shore half a
            // metre from where a player wading into it actually gets their feet wet.
            let under = level - terrain.surface_at(*ix, *iz);
            depth[b * xs.len() + a] = under;
            wet |= under > 0.0;
        }
    }
    if !wet {
        return None;
    }

    // Vertices are emitted lazily: a map with one wet corner contributes four of them, and the dry
    // half of a map contributes none.
    let mut vertex_of = vec![u32::MAX; xs.len() * zs.len()];
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut depths: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut emit = |a: usize, b: usize, positions: &mut Vec<[f32; 3]>, uvs: &mut Vec<[f32; 2]>, depths: &mut Vec<[f32; 2]>| {
        let node = b * xs.len() + a;
        if vertex_of[node] != u32::MAX {
            return vertex_of[node];
        }
        let at = grid.world_of(xs[a], zs[b]);
        let index = positions.len() as u32;
        positions.push([at.x, level, at.y]);
        uvs.push([at.x, at.y]);
        // The depth rides in the second uv set for the reason the ground's dip does: it is a slot
        // the standard vertex shader already carries through to the fragment stage, and adding a
        // channel of my own would mean replacing that shader whole to hand over one float.
        depths.push([depth[node], 0.0]);
        vertex_of[node] = index;
        index
    };

    for b in 0..zs.len().saturating_sub(1) {
        for a in 0..xs.len().saturating_sub(1) {
            let corners = [
                depth[b * xs.len() + a],
                depth[b * xs.len() + a + 1],
                depth[(b + 1) * xs.len() + a],
                depth[(b + 1) * xs.len() + a + 1],
            ];
            if corners.iter().all(|under| *under <= 0.0) {
                continue;
            }
            let v00 = emit(a, b, &mut positions, &mut uvs, &mut depths);
            let v10 = emit(a + 1, b, &mut positions, &mut uvs, &mut depths);
            let v01 = emit(a, b + 1, &mut positions, &mut uvs, &mut depths);
            let v11 = emit(a + 1, b + 1, &mut positions, &mut uvs, &mut depths);
            // The same winding the ground is built with: counter-clockwise seen from above.
            indices.extend_from_slice(&[v00, v01, v10, v10, v01, v11]);
        }
    }
    if indices.is_empty() {
        return None;
    }

    let normals = vec![[0.0f32, 1.0, 0.0]; positions.len()];
    Some(
        Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        // Flat, every one of them. The waves are a perturbation the shader applies per pixel, and
        // baking them into the mesh would freeze one moment of a moving surface into the geometry.
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, depths)
        .with_inserted_indices(bevy::mesh::Indices::U32(indices)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::terrain::default_terrain;

    fn flat(level: Option<f32>, height: f32) -> Terrain {
        let mut map = Terrain::new(64.0, 64.0, 1.0, -32.0, 32.0).expect("a map within the caps");
        let sample = map.grid.quantise(height);
        map.heights.fill(sample);
        map.water_y = level;
        map
    }

    /// A dry map has no surface at all, and neither has one whose water is under the ground.
    ///
    /// Two ways of saying "there is nothing to draw", and they have to be one answer: a mesh with
    /// no wet vertex in it is a sheet of fully transparent water covering the whole map, drawn and
    /// blended every frame for nothing.
    #[test]
    fn nothing_is_drawn_where_there_is_no_water() {
        assert!(water_mesh(&flat(None, 0.0)).is_none(), "a dry map");
        assert!(water_mesh(&flat(Some(-4.0), 0.0)).is_none(), "water below every sample");
    }

    /// Only the submerged part of the map gets quads.
    ///
    /// Half the map at -5 and half at +5 with the water at 0: the dry half must cost nothing. This
    /// is the property that makes a pond a pond rather than a map-sized plane, and it is invisible
    /// on screen either way — the shader fades the dry half out — so nothing but a test would
    /// notice it stopping working.
    #[test]
    fn a_pond_costs_a_pond() {
        let mut map = flat(Some(0.0), -5.0);
        let grid = map.grid;
        for iz in 0..grid.nz {
            for ix in grid.nx / 2..grid.nx {
                let index = grid.index(ix, iz);
                map.heights[index] = grid.quantise(5.0);
            }
        }
        let mesh = water_mesh(&map).expect("half of it is under water");
        let whole = water_mesh(&flat(Some(0.0), -5.0)).expect("all of it is");
        let (part, all) = (mesh.count_vertices(), whole.count_vertices());
        assert!(part < all * 3 / 5, "the flooded half wants {part} vertices of the map's {all}");
    }

    /// Every vertex carries how deep the water is under it, which is the whole of what the shader
    /// reads.
    ///
    /// Measured against the *surface* rather than the authored height, because that is what a
    /// player wades into: the roughening moves the ground under the sea as much as it does anywhere
    /// else.
    #[test]
    fn each_vertex_says_how_deep_the_water_is_under_it() {
        let map = flat(Some(2.0), -1.0);
        let mesh = water_mesh(&map).expect("a flooded map");
        let Some(bevy::mesh::VertexAttributeValues::Float32x2(depths)) =
            mesh.attribute(Mesh::ATTRIBUTE_UV_1)
        else {
            panic!("the depth is not where the shader reads it");
        };
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(points)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the surface has no positions");
        };
        assert_eq!(depths.len(), points.len());
        for (point, depth) in points.iter().zip(depths) {
            assert!((point[1] - 2.0).abs() < 1e-4, "the sheet is not flat: {}", point[1]);
            let under = 2.0 - map.height_over(point[0], point[2]);
            assert!(
                (depth[0] - under).abs() < 0.2,
                "a vertex says {:.2} m of water where the ground is {under:.2} m down",
                depth[0],
            );
        }
    }

    /// The sheet reaches the edge of the map, whatever the step works out to.
    ///
    /// The default map's axes are not a multiple of the reduction, so the last row of samples is
    /// only reached because [`axis`] adds it. Without that there is a strip of ground along two
    /// sides of every flooded map with no water on it and nothing to explain why.
    #[test]
    fn the_sheet_reaches_the_last_sample() {
        let mut map = default_terrain();
        let (low, high) = map.grid.bounds();
        map.water_y = Some(map.grid.max_y);
        let mesh = water_mesh(&map).expect("a map flooded to the ceiling");
        let Some(bevy::mesh::VertexAttributeValues::Float32x3(points)) =
            mesh.attribute(Mesh::ATTRIBUTE_POSITION)
        else {
            panic!("the surface has no positions");
        };
        let reach = |pick: fn(&[f32; 3]) -> f32, want: f32| {
            assert!(
                points.iter().any(|point| (pick(point) - want).abs() < 1e-3),
                "no vertex sits on {want}",
            );
        };
        reach(|point| point[0], low.x);
        reach(|point| point[0], high.x);
        reach(|point| point[2], low.y);
        reach(|point| point[2], high.y);
    }
}
