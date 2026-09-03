//! The lawn: where grass grows, and what a tuft of it is made of.
//!
//! **Grass is derived and stored nowhere.** There is no scatter map, nothing is placed by hand, and
//! the server never hears that grass exists — no entity, no collider, no field in any message. What
//! decides where a tuft stands is the same rule that decides what the ground is painted with:
//! [`Layer::grass`](noob_tube_shared::terrain::Layer::grass) says how thickly a surface grows, and
//! the layer's own weight at a point thins it out. Sculpt a hillside steeper and the lawn fades into
//! rock exactly where the texture does, because it is the same weight doing both.
//!
//! **Blades, not textured cross-quads.** The usual cheap grass is two crossed quads wearing an
//! alpha-tested foliage texture, and it is wrong here for the reason the predecessor project
//! (`../webgame`, `client/src/grass.ts`) gives: this repository has no foliage texture — every pack
//! in `assets/textures` is ground or wall — and alpha is exactly the channel a texture-reducing
//! tool is entitled to throw away. Real geometry needs no asset and no alpha test. It costs
//! triangles instead, which the density and the draw distance control.
//!
//! **A cell at a time, near the camera only.** A 512 m map at eight tufts a square metre is two
//! million tufts, which is not a thing to build. The lawn is grown in eight-metre cells within
//! [`REACH`] of the eye, a couple of cells a frame, and each one is a single mesh with its tufts
//! baked into it — so a cell is one draw call rather than five hundred entities, and the ECS never
//! sees a blade of grass.

use bevy::asset::uuid_handle;
use bevy::camera::visibility::VisibilityRange;
use bevy::light::NotShadowCaster;
use bevy::platform::collections::HashMap;
use bevy::prelude::*;
use noob_tube_shared::sculpt::GroundPatched;
use noob_tube_shared::terrain::{Ground, Installed, Terrain, slope_degrees};
use noob_tube_shared::types::Authored;

pub struct GrassPlugin;

impl Plugin for GrassPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Lawn>().add_systems(Startup, mix_the_grass).add_systems(
            Update,
            (turn_over_the_lawn, mow_what_moved, grow_the_lawn)
                .chain()
                .run_if(resource_exists::<Ground>),
        );
    }
}

/// How wide one grown patch of lawn is, in metres.
///
/// The unit everything here works in: what is built at once, what is thrown away when the ground
/// under it moves, and what fades as one. Small enough that a stroke of the brush costs a handful
/// of them and that the fade does not happen to a whole hillside at a time; large enough that the
/// lawn is a few dozen meshes rather than a few thousand.
const CELL: f32 = 8.0;

/// How far from the eye grass is grown, in metres, and where it fades out.
///
/// The fade is chosen by how *big* a tuft still is when it goes rather than by how far away it is.
/// Past twenty-odd metres a tuft covers a pixel or two, and a lawn of one-pixel specks does not
/// read as grass — it reads as noise crawling over the ground, which is worse than the ground's own
/// texture, which is a photograph of grass. So it ends while the blades are still blades.
///
/// [`REACH`] has to sit outside the end of the fade, or the fade would be cut off by a cell
/// appearing and the point of it lost.
///
/// **This is the knob that costs.** The lawn is triangles, and how many of them there are goes with
/// the square of this: sixteen metres against twenty-six is a third of the grass. It was set by
/// measuring — twenty-six metres at eight tufts a square metre was 18 frames a second on the
/// machine this game is aimed at, against 45 with no grass at all — and by then spending what was
/// left on density instead of distance, because a hole in the lawn at your feet is what you see and
/// a lawn that ends twenty-six metres away is not.
const REACH: f32 = 16.0;
const FADE_FROM: f32 = 11.0;
const FADE_TO: f32 = 15.0;

/// How many cells may be grown in one frame.
///
/// A cell is about a millisecond of work — placing five hundred tufts and asking the height field
/// where each of them stands — and there is no reason to pay for a dozen of them at once. Walking
/// forward at running speed crosses a cell in a second and a half, so two a frame is far more than
/// keeps up; what this really bounds is the moment the lawn first appears, or a map arrives.
const CELLS_PER_FRAME: usize = 2;

/// One tuft: how many blades, how tall, how wide, and how far they stand apart.
///
/// **A wider blade is cheaper than another one**, and that is the whole of how this was tuned: the
/// cost of the lawn is its triangles, three quarters of which are shading a blade that is a couple
/// of pixels across. Three blades at six centimetres cover what four at four and a half did, for
/// three quarters of the geometry.
///
/// Short on purpose: this is a lawn, not a meadow, and the height is what decides whether the grass
/// hides the ground it grows out of.
const BLADES: usize = 3;
const BLADE_HIGH: f32 = 0.26;
const BLADE_WIDE: f32 = 0.05;
const SPREAD: f32 = 0.10;

/// The colour at a blade's root and at its tip, in **linear** light.
///
/// Deliberately darker than the ground texture at the base and only a little lighter at the tip.
/// Grass brighter than what it grows out of reads as scattered *on* the ground rather than as part
/// of it — the speckled look that gives a scatter system away at a glance. The root is within a
/// hair of `Grass001`'s own average, which is the colour the ground underneath is painted.
///
/// Measured against the ground rather than guessed at: on this map's sun the drawn ground sits at
/// about (107, 142, 87) on screen, and a tip is meant to land a little above that and not at (175,
/// 194, 138), which is what the predecessor's own numbers came out at here and which reads as pale
/// straw scattered over green. Its palette was right about the *shape* — dark root, lighter tip,
/// neither brighter than the ground by much — and wrong about the level, because a colour is only
/// half of what a pixel is and the other half is somebody else's sun.
const ROOT: [f32; 3] = [0.035, 0.075, 0.015];
const TIP: [f32; 3] = [0.13, 0.24, 0.050];

/// The one material every tuft on the map wears.
const GRASS_MATERIAL: Handle<StandardMaterial> =
    uuid_handle!("3c9e1d70-5a44-4f81-8f2b-6e0d7c41a952");

/// What is standing, and which map it belongs to.
///
/// A cell that grows nothing — rock, or a map whose layers ask for no grass — is remembered as
/// `None` rather than left out. Without that it would be tried again every frame for as long as it
/// is in reach, which is the one case with no work to show for the asking.
#[derive(Resource, Default)]
struct Lawn {
    grown: HashMap<(i32, i32), Option<Entity>>,
    /// The map the cells above were grown from, so that a different one takes them with it.
    map: Option<Installed>,
}

/// Update: throws the whole lawn away when the map underneath it is a different one.
///
/// A map switch changes every height on the field, so there is nothing to keep: this is the one
/// case where growing it all again is right. [`Installed`] is what tells a new map from an edited
/// one — see the note on it, and `build_the_ground` for the same question asked about colliders.
fn turn_over_the_lawn(ground: Res<Ground>, mut lawn: ResMut<Lawn>, mut commands: Commands) {
    if lawn.map == Some(ground.installed()) {
        return;
    }
    lawn.map = Some(ground.installed());
    for (_, entity) in lawn.grown.drain() {
        if let Some(entity) = entity {
            commands.entity(entity).despawn();
        }
    }
}

/// Update: takes up the cells a stroke moved the ground under.
///
/// Taken up rather than rebuilt, because [`grow_the_lawn`] will grow them again on the next frame
/// or two if they are still near the eye — and if they are not, rebuilding them would be work for
/// grass nobody can see. A tuft stands on the surface, so a cell whose ground has moved is a cell
/// of grass hanging in the air or buried to the tip.
fn mow_what_moved(
    ground: Res<Ground>,
    mut patched: MessageReader<GroundPatched>,
    mut lawn: ResMut<Lawn>,
    mut commands: Commands,
) {
    for GroundPatched(patch) in patched.read() {
        let (low, high) = patch.bounds(&ground.0.grid);
        // A tuft leans and its blades spread, so a cell reaches a little past its own edge; and a
        // stroke's rim moves the ground by nothing at all, where a cell either side of it does not
        // care. One cell of margin covers both.
        let (from, to) = (
            ((low - Vec2::splat(CELL)) / CELL).floor(),
            ((high + Vec2::splat(CELL)) / CELL).ceil(),
        );
        lawn.grown.retain(|(cx, cz), entity| {
            let inside = (*cx as f32) >= from.x
                && (*cx as f32) <= to.x
                && (*cz as f32) >= from.y
                && (*cz as f32) <= to.y;
            if inside && let Some(entity) = entity {
                commands.entity(*entity).despawn();
            }
            !inside
        });
    }
}

/// Update: grows the cells near the eye and takes up the ones behind you.
///
/// The distance test runs every frame and is a few dozen comparisons; the building is budgeted,
/// nearest first, so walking into fresh ground grows the cell you are about to be standing on
/// before the one at the edge of sight.
fn grow_the_lawn(
    ground: Res<Ground>,
    camera: Option<Single<&GlobalTransform, With<Camera3d>>>,
    root: Single<Entity, With<crate::world::LevelRoot>>,
    mut lawn: ResMut<Lawn>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    let Some(camera) = camera else {
        return;
    };
    let eye = camera.translation();

    // Gone from under your feet: a cell is thrown away as soon as it is out of reach, which is
    // past the end of the fade, so nothing ever vanishes while it can still be seen.
    lawn.grown.retain(|cell, entity| {
        let keep = flat_distance(centre(*cell), eye) <= REACH + CELL;
        if !keep && let Some(entity) = entity {
            commands.entity(*entity).despawn();
        }
        keep
    });

    let mut wanted: Vec<((i32, i32), f32)> = Vec::new();
    let span = (REACH / CELL).ceil() as i32;
    let (here_x, here_z) = ((eye.x / CELL).floor() as i32, (eye.z / CELL).floor() as i32);
    for cz in here_z - span..=here_z + span {
        for cx in here_x - span..=here_x + span {
            if lawn.grown.contains_key(&(cx, cz)) {
                continue;
            }
            let away = flat_distance(centre((cx, cz)), eye);
            if away <= REACH {
                wanted.push(((cx, cz), away));
            }
        }
    }
    if wanted.is_empty() {
        return;
    }
    // Nearest first: the cell you are about to walk into matters more than the one at the horizon
    // of the lawn, and with a budget of two a frame the order is what you actually see.
    wanted.sort_by(|a, b| a.1.total_cmp(&b.1));

    for (cell, _) in wanted.into_iter().take(CELLS_PER_FRAME) {
        let at = centre(cell);
        let stands = Vec3::new(at.x, ground.0.height_over(at.x, at.y), at.y);
        let grown = cell_mesh(&ground.0, cell, stands).map(|mesh| {
            commands
                .spawn((
                    Name::from(format!("Grass {},{}", cell.0, cell.1)),
                    Authored,
                    Mesh3d(meshes.add(mesh)),
                    MeshMaterial3d(GRASS_MATERIAL),
                    // A lawn does not cast shadows, and the reason is cost rather than taste: the
                    // shadow pass draws every caster again, so grass that casts doubles the most
                    // expensive geometry on the screen to draw the one thing it is worst at —
                    // hundreds of thousands of blade-sized triangles resolved into a shadow map
                    // that has one texel per several blades. Measured: it was most of the frame.
                    // What is lost is grass shading itself, which at this height is a darkening
                    // the ambient term already gives.
                    NotShadowCaster,
                    Transform::from_translation(stands),
                    // The fade is on the entity's own origin rather than its bounding box, which is
                    // why the cell stands at the height of the ground under its middle: with the
                    // origin at y = 0 a lawn on a hilltop would measure its distance to the camera
                    // through the hill.
                    VisibilityRange {
                        start_margin: 0.0..0.0,
                        end_margin: FADE_FROM..FADE_TO,
                        use_aabb: false,
                    },
                    ChildOf(*root),
                ))
                .id()
        });
        lawn.grown.insert(cell, grown);
    }
}

/// Where a cell's middle is, in world x and z.
fn centre(cell: (i32, i32)) -> Vec2 {
    Vec2::new((cell.0 as f32 + 0.5) * CELL, (cell.1 as f32 + 0.5) * CELL)
}

/// How far apart two places are on the ground, ignoring height.
///
/// Ignoring it is the point: grass is grown around where you *are*, and a player on a fifty-metre
/// tower should not lose the lawn under their own feet.
fn flat_distance(at: Vec2, eye: Vec3) -> f32 {
    (at - Vec2::new(eye.x, eye.z)).length()
}

/// One cell of lawn, as a single mesh with its tufts baked into it.
///
/// `None` when nothing grows here, which is the ordinary answer over rock and over any map whose
/// layers ask for no grass at all.
///
/// **Placement is a jittered grid, not a scatter.** Uniformly random points clump and leave holes —
/// that is what uniformly random means — and a hole in a lawn is the one thing a lawn must not
/// have. So the cell is divided into as many squares as it wants tufts and one tuft is placed
/// inside each, jittered, which is even coverage and randomness at the same time.
///
/// Everything about a tuft comes from a hash of *where it is*, so a cell rebuilt after a stroke
/// grows back exactly the lawn that was there rather than a new one, and two players standing in
/// the same field see the same blades.
fn cell_mesh(terrain: &Terrain, cell: (i32, i32), origin: Vec3) -> Option<Mesh> {
    let thickest = terrain.layers.iter().map(|layer| layer.grass).fold(0.0, f32::max);
    if thickest <= 0.0 {
        return None;
    }
    // One square per tuft the thickest layer would grow, so that thinning is a matter of leaving
    // some of them out rather than of moving the rest.
    let across = (CELL * thickest.sqrt()).round().max(1.0) as i32;
    let step = CELL / across as f32;
    let (x0, z0) = (cell.0 as f32 * CELL, cell.1 as f32 * CELL);

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut colours: Vec<[f32; 4]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for row in 0..across {
        for column in 0..across {
            let seed = mix(
                (cell.0.wrapping_mul(73_856_093) ^ cell.1.wrapping_mul(19_349_663)) as u32,
                (row.wrapping_mul(83_492_791) ^ column) as u32,
            );
            let x = x0 + (column as f32 + fraction(seed)) * step;
            let z = z0 + (row as f32 + fraction(mix(seed, 1))) * step;
            // The thinning: this ground wants `density` tufts a square metre where the grid offers
            // `thickest` of them, so this many out of every hundred stand.
            if fraction(mix(seed, 2)) * thickest > density_at(terrain, x, z) {
                continue;
            }
            tuft(&mut positions, &mut normals, &mut colours, &mut indices, terrain, origin, x, z, seed);
        }
    }
    if indices.is_empty() {
        return None;
    }
    Some(
        Mesh::new(
            bevy::mesh::PrimitiveTopology::TriangleList,
            bevy::asset::RenderAssetUsages::default(),
        )
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_COLOR, colours)
        .with_inserted_indices(bevy::mesh::Indices::U32(indices)),
    )
}

/// How many tufts a square metre of ground at this point wants.
///
/// The layer rule, and nothing else: every layer's weight here, times what that layer grows. It is
/// the same function the shader evaluates to decide what colour this ground is, so the lawn and the
/// texture cannot disagree about where the grass ends.
fn density_at(terrain: &Terrain, x: f32, z: f32) -> f32 {
    let grid = terrain.grid;
    let y = terrain.height_over(x, z);
    let reach = grid.spacing;
    // The surface normal from central differences of the surface itself, which is what the drawn
    // ground is and therefore what a tuft stands on.
    let (east, west) = (terrain.height_over(x + reach, z), terrain.height_over(x - reach, z));
    let (north, south) = (terrain.height_over(x, z + reach), terrain.height_over(x, z - reach));
    let normal = Vec3::new(west - east, 2.0 * reach, south - north).normalize_or(Vec3::Y);
    // The hollow rule reads the field at samples rather than between them, so this asks at the
    // nearest one; a tuft is a tenth of a sample across and the difference cannot be seen.
    let ix = ((x - grid.origin_x) / grid.spacing).round().clamp(0.0, (grid.nx - 1) as f32) as u32;
    let iz = ((z - grid.origin_z) / grid.spacing).round().clamp(0.0, (grid.nz - 1) as f32) as u32;
    let dip = terrain.dip_at(ix, iz);
    let slope = slope_degrees(normal.y);
    terrain.layers.iter().map(|layer| layer.weight(slope, y, dip) * layer.grass).sum()
}

/// One tuft, appended to the buffers a cell is being built in.
///
/// Each blade is a strip of three triangles that narrows and leans as it rises. The lean is what
/// stops a tuft reading as a fan of straight spikes — real grass falls away from its own centre.
#[allow(clippy::too_many_arguments)]
fn tuft(
    positions: &mut Vec<[f32; 3]>,
    normals: &mut Vec<[f32; 3]>,
    colours: &mut Vec<[f32; 4]>,
    indices: &mut Vec<u32>,
    terrain: &Terrain,
    origin: Vec3,
    x: f32,
    z: f32,
    seed: u32,
) {
    let foot = Vec3::new(x, terrain.height_over(x, z), z) - origin;
    // A tuft's own turn and its own size. Without the first, every clump on the map faces the same
    // way and the lawn shows the grid it was placed on from directly above.
    let turn = fraction(mix(seed, 3)) * std::f32::consts::TAU;
    let size = 0.75 + fraction(mix(seed, 4)) * 0.5;

    for blade in 0..BLADES {
        // Not an even fan: evenly spaced blades read as a machined star from above.
        let yaw = turn
            + blade as f32 / BLADES as f32 * std::f32::consts::TAU
            + if blade % 2 == 0 { 0.35 } else { -0.2 };
        let (sin, cos) = yaw.sin_cos();
        // Blades differ from each other, and the difference is worked out from the blade's number
        // rather than drawn from the hash: a tuft wants variety, not noise.
        let out = SPREAD * (0.35 + (blade % 3) as f32 * 0.32);
        let lean = (0.20 + (blade % 3) as f32 * 0.09) * size;
        let high = BLADE_HIGH * size * (0.62 + (blade % 4) as f32 * 0.13);
        let wide = BLADE_WIDE * size * (0.85 + (blade % 3) as f32 * 0.12);

        let base = positions.len() as u32;
        let mut at = |across: f32, up: f32, along: f32| {
            let out = along + out;
            positions.push(
                (foot + Vec3::new(across * cos - out * sin, up, across * sin + out * cos))
                    .to_array(),
            );
            // Mostly upright normals. A blade's true normal faces sideways, and under a sun that
            // comes from above that renders the whole lawn near-black; leaning them up makes grass
            // catch the same light as the ground it grows out of.
            let normal = Vec3::new(-sin * 0.25, 1.0, cos * 0.25).normalize();
            normals.push(normal.to_array());
            let t = up / high;
            colours.push([
                ROOT[0] + (TIP[0] - ROOT[0]) * t,
                ROOT[1] + (TIP[1] - ROOT[1]) * t,
                ROOT[2] + (TIP[2] - ROOT[2]) * t,
                1.0,
            ]);
        };
        at(-wide * 0.5, 0.0, 0.0);
        at(wide * 0.5, 0.0, 0.0);
        at(-wide * 0.34, high * 0.55, lean * 0.35);
        at(wide * 0.34, high * 0.55, lean * 0.35);
        at(0.0, high, lean);
        indices.extend_from_slice(&[
            base,
            base + 1,
            base + 2,
            base + 2,
            base + 1,
            base + 3,
            base + 2,
            base + 3,
            base + 4,
        ]);
    }
}

/// Startup: the material every blade wears.
///
/// One for the whole map, because everything that differs between two tufts is in the mesh: the
/// colour is a root-to-tip gradient written per vertex, and the shape is baked where it stands.
fn mix_the_grass(mut materials: ResMut<Assets<StandardMaterial>>) {
    let written = materials.insert(
        &GRASS_MATERIAL,
        StandardMaterial {
            // White: the colour is in the mesh, a root-to-tip gradient per blade, and a base colour
            // that was anything else would tint it.
            base_color: Color::WHITE,
            perceptual_roughness: 0.95,
            // A blade is one triangle thick, so its back is half of what you see — and the two
            // settings below are not the same thing said twice. `cull_mode: None` is what draws the
            // back at all. `double_sided` would additionally *flip the normal* on it, which is
            // right for a leaf and wrong for a blade of grass: the normals here are deliberately
            // tipped up so that a lawn catches the light the ground catches, and flipping one
            // points it at the earth. Measured with it on: a tenth of every lawn pixel was five
            // times darker than the ground around it — black shards lying on the grass.
            double_sided: false,
            cull_mode: None,
            ..default()
        },
    );
    if let Err(error) = written {
        error!("the grass material could not be written: {error}");
    }
}

/// A hash, and the two things made out of it.
///
/// Integer arithmetic rather than a random number generator, because a tuft has to come back the
/// same after the cell it is in has been thrown away and grown again — a stroke of the brush at the
/// far end of a cell must not reshuffle the lawn at this end. A generator would need its state
/// carried and its draws counted; a hash of the place needs neither.
fn mix(a: u32, b: u32) -> u32 {
    let mut h = a ^ b.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h.wrapping_mul(0x846c_a68b) ^ (h >> 13)
}

/// The hash as a number from 0 to 1, over 24 bits, which is every value an `f32` holds exactly in
/// that range.
fn fraction(h: u32) -> f32 {
    (h >> 8) as f32 / (1u32 << 24) as f32
}
