//! The level, as this client has it: the map it was sent, and what all of it looks like.
//!
//! Where the built geometry *is* lives in `noob_tube_shared::level`, so the server collides against
//! the same numbers, and this module only turns those into meshes. The ground is the exception and
//! the reason this module is not simply "what the level looks like": it is not a constant either
//! side can build, it is a map the server owns and sends, and until it arrives this client has no
//! ground at all — see [`adopt_the_map`].

use bevy::prelude::*;
use lightyear::prelude::{MessageReceiver, MessageSystems, Predicted};
use noob_tube_shared::level::{self, CRATE_HALF_EXTENT, RAMP_HALF_EXTENTS};
use noob_tube_shared::sculpt::{self, GroundPatched, PendingEdits, TerrainEdit};
use noob_tube_shared::terrain::{Ground, MarkerChanged, Palette, Terrain, TerrainBaseline};
use noob_tube_shared::types::Authored;

pub struct WorldPlugin;

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        app.register_type::<LevelRoot>()
            .add_systems(Startup, (spawn_ground, level::spawn_level))
            // After lightyear has put what arrived into the receivers, and before `FixedMain`
            // runs — both halves matter. Ordering it before `MessageSystems::Receive` reads an
            // inbox that is always empty, which lightyear then says out loud once a second:
            // "Unhandled messages ... Clearing to avoid accumulating messages". And building the
            // collider in `Update` instead would be a tick of physics late, because `FixedMain`
            // runs first inside a frame.
            .init_resource::<PendingEdits>()
            .add_message::<GroundPatched>()
            .add_systems(
                PreUpdate,
                (
                    adopt_the_map,
                    take_strokes,
                    // Before the ground is rebuilt, because that is what a placement changes: both
                    // the colliders and the picture follow from the map having changed, so a
                    // marker reaches the world through the same door a map switch does.
                    take_placements.run_if(resource_exists::<Ground>),
                    level::build_the_ground.run_if(resource_exists_and_changed::<Ground>),
                    level::build_the_props.run_if(resource_exists_and_changed::<Ground>),
                    draw_the_props.run_if(resource_exists_and_changed::<Ground>),
                    sculpt::apply_due_edits.run_if(resource_exists::<Ground>),
                    sculpt::lift_with_the_ground::<With<Predicted>>
                        .run_if(resource_exists::<Ground>),
                    // Every vehicle this client places, not only a predicted one: an interpolated
                    // vehicle's pose is overwritten by the next snapshot either way, and the one
                    // being driven is exactly the one that must not be left under a hillside.
                    sculpt::lift_bodies_with_the_ground::<()>.run_if(resource_exists::<Ground>),
                    level::rebuild_patched_ground.run_if(resource_exists::<Ground>),
                )
                    .chain()
                    .after(MessageSystems::Receive),
            )
            .add_systems(
                Update,
                dress_the_ground.run_if(resource_exists_and_changed::<Ground>),
            )
            // The picture of a stroke, after the ground it describes has moved. In `Update` rather
            // than `PreUpdate` because it is only a picture: the collider goes first, and a frame
            // of the two disagreeing would be a frame of walking on ground you cannot see.
            .add_systems(
                Update,
                redress_patched_tiles
                    .after(dress_the_ground)
                    .run_if(resource_exists::<Ground>),
            );
    }
}

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
fn ground_mesh(terrain: &Terrain, tx: u32, tz: u32) -> Mesh {
    let grid = terrain.grid;
    let (ix0, ix1, iz0, iz1) = grid.tile_samples(tx, tz);
    let step = MESH_STRIDE.max(1);
    let (across, down) = ((ix1 - ix0) / step, (iz1 - iz0) / step);
    let (wide, deep) = ((across + 1) as usize, (down + 1) as usize);
    let mut positions = Vec::with_capacity(wide * deep);
    let mut normals: Vec<[f32; 3]> = Vec::with_capacity(wide * deep);
    let mut uvs = Vec::with_capacity(wide * deep);
    let mut dips = Vec::with_capacity(wide * deep);

    for row in 0..=down {
        for column in 0..=across {
            let (ix, iz) = (ix0 + column * step, iz0 + row * step);
            let here = grid.world_of(ix, iz);
            // `surface_at`, not `height_at`: the authored field is what the map stores, and the
            // surface is that plus the relief steep ground carries. The collider is built from the
            // same call, which is the whole reason the relief lives in `Terrain` rather than here.
            positions.push([here.x, terrain.surface_at(ix, iz), here.y]);
            // Central differences, one sample either side, falling back to this sample at the rim
            // of the map. The cross product of the two tangents comes out as this without building
            // them.
            let west = terrain.surface_at(ix.saturating_sub(step), iz);
            let east = terrain.surface_at((ix + step).min(grid.nx - 1), iz);
            let south = terrain.surface_at(ix, iz.saturating_sub(step));
            let north = terrain.surface_at(ix, (iz + step).min(grid.nz - 1));
            let normal = Vec3::new(west - east, 2.0 * step as f32 * grid.spacing, south - north);
            normals.push(normal.normalize().into());
            uvs.push([here.x, here.y]);
            // How far this point sits below the ground around it, which is the third thing the
            // shader derives the surface from. It rides in the second uv set because that is a
            // slot the standard vertex shader already carries through to the fragment stage —
            // adding a channel of my own would mean replacing that shader whole, to hand over one
            // float. It belongs here rather than in a texture for the same reason the normal does:
            // it is a function of the height field, so a tile that rebuilds its mesh rebuilds this
            // in the same pass, and the two can never disagree about the ground they describe.
            dips.push([terrain.dip_at(ix, iz), 0.0]);
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
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, dips)
    .with_inserted_indices(bevy::mesh::Indices::U32(indices))
}

/// One tile's rock as one mesh, or nothing if the tile has none.
///
/// **Read out of the collider, not built beside it.** `rock::tile_collider` and this go through the
/// same `Collider::convex_hull`, and the faces drawn here are that hull's own faces — so the block
/// you see is the block you bump into, not a second model of it. `shape_scaled` gives the built
/// shape back; a hull knows its faces, which vertices are on each of them and which way each one
/// points, which is everything a mesh needs and more than a triangle soup would have said.
///
/// **One mesh a tile, not one a rock.** A face of a hill can carry a few hundred, and a few hundred
/// entities with a few hundred draw calls is how you spend a frame on gravel. Merged, a tileful of
/// rock costs what the tile's ground costs: one.
///
/// The vertices are in world space, like the ground's, so the entity sits at the identity and a
/// rock's position is the same number here as in its collider. Each face gets its own copies of its
/// corners carrying the face's normal — which is the definition of flat shading, and the only way
/// to get it, since a shared vertex can only carry one normal and a corner of a rock is on three
/// faces pointing three ways.
fn rock_mesh(terrain: &Terrain, tx: u32, tz: u32) -> Option<Mesh> {
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut dips: Vec<[f32; 2]> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    for rock in noob_tube_shared::rock::rocks_of_tile(terrain, tx, tz) {
        let Some(collider) = rock.collider() else {
            continue;
        };
        let Some(hull) = collider.shape_scaled().as_convex_polyhedron() else {
            continue;
        };
        let corners = hull.points();
        let on_face = hull.vertices_adj_to_face();
        for face in hull.faces() {
            let first = face.first_vertex_or_edge as usize;
            let count = face.num_vertices_or_edges as usize;
            if count < 3 {
                continue;
            }
            let facing = rock.facing * face.normal;
            let base = positions.len() as u32;
            for step in 0..count {
                let at = rock.at + rock.facing * corners[on_face[first + step] as usize];
                positions.push(at.to_array());
                normals.push(facing.to_array());
                uvs.push([at.x, at.z]);
                // No hollow. `dip_at` measures a valley twelve metres wide, and a boulder is not
                // one — asking the ground under it would paint the dirt of the gully it sits in
                // over the stone itself.
                dips.push([0.0, 0.0]);
            }
            // A fan, which is enough: every face of a convex hull is a convex polygon, and its
            // corners come back wound the one way round.
            for step in 1..count as u32 - 1 {
                indices.extend_from_slice(&[base, base + step, base + step + 1]);
            }
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
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_1, dips)
        .with_inserted_indices(bevy::mesh::Indices::U32(indices)),
    )
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

/// What the map's own props hang under, so a map switch knows what to take down.
///
/// A component rather than a stored `Entity`, for the same reason [`LevelRoot`] is one: the
/// drawing runs when the map arrives, which is long after the root was made, and a resource
/// holding an id is a second place for it to be wrong.
#[derive(Component, Reflect, Default)]
#[reflect(Component)]
pub struct PropsRoot;

/// PreUpdate: takes the map the server sent and makes it this client's ground.
///
/// The absence of [`Ground`] is what "no map yet" means, so this inserting it is the whole of the
/// handover: the collider system and the mesh system both watch that resource and neither knows
/// where it came from. That is what will let step six swap maps mid-round without either of them
/// changing.
///
/// A baseline that does not decode is refused and logged rather than trusted. It arrived from
/// another machine, and [`TerrainBaseline::adopt`] puts it through the same caps a map read off
/// disk faces — a grid claiming four million samples is an allocation this client should not make
/// because somebody asked it to.
fn adopt_the_map(
    mut inbox: Query<&mut MessageReceiver<TerrainBaseline>>,
    mut pending: ResMut<PendingEdits>,
    mut commands: Commands,
) {
    for mut receiver in inbox.iter_mut() {
        for baseline in receiver.receive() {
            match baseline.adopt() {
                Ok(terrain) => {
                    info!(
                        "map received: {}x{} samples at {} m, {} strokes still on their way",
                        terrain.grid.nx,
                        terrain.grid.nz,
                        terrain.grid.spacing,
                        baseline.pending.len(),
                    );
                    commands.insert_resource(Ground(terrain));
                    // What this server can place. Empty means an older one, and the client's own
                    // list is a better answer to that than a palette that can place nothing.
                    if !baseline.palette.is_empty() {
                        commands.insert_resource(Palette(baseline.palette.clone()));
                    }
                    // Strokes accepted before this client arrived but not yet applied. The ground
                    // it was just given has not had them, and without them it never would — this
                    // client would be the only one standing on a map without somebody's hill.
                    pending.0 = baseline.pending.clone();
                }
                Err(fault) => error!("the map the server sent is not one this build reads: {fault}"),
            }
        }
    }
}

/// PreUpdate: takes the strokes the server has accepted and queues them for their tick.
///
/// Queued rather than applied, and that is the whole of the scheme: the server said which tick this
/// lands on, and applying it a moment earlier because it happened to arrive early would put this
/// client on ground nobody else has yet.
fn take_strokes(mut inbox: Query<&mut MessageReceiver<TerrainEdit>>, mut pending: ResMut<PendingEdits>) {
    for mut receiver in inbox.iter_mut() {
        for edit in receiver.receive() {
            pending.0.push(edit);
        }
    }
}

/// Update: rebuilds the meshes of the tiles a stroke moved, and only those.
///
/// The same tiling the collider uses, from the same function, so the ground you see and the ground
/// you stand on are rebuilt over exactly the same samples.
fn redress_patched_tiles(
    ground: Res<Ground>,
    mut patched: MessageReader<GroundPatched>,
    tiles: Query<(&GroundTile, &Mesh3d)>,
    rocks: Query<(Entity, &RockTile)>,
    root: Single<Entity, With<LevelRoot>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut commands: Commands,
) {
    // The constant handle the material is always written under, rather than the one a tile happens
    // to be holding: a rebuild has to reach it even in the frame before any tile exists.
    let material = crate::ground_material::GROUND_MATERIAL;
    let mut dirty: Vec<(u32, u32)> = Vec::new();
    for GroundPatched(patch) in patched.read() {
        let (tx0, tx1, tz0, tz1) =
            ground.0.grid.tiles_over(patch.ix0, patch.ix1, patch.iz0, patch.iz1);
        for tz in tz0..=tz1 {
            for tx in tx0..=tx1 {
                if !dirty.contains(&(tx, tz)) {
                    dirty.push((tx, tz));
                }
            }
        }
    }
    if dirty.is_empty() {
        return;
    }
    for (tile, handle) in tiles.iter() {
        if !dirty.contains(&(tile.tx, tile.tz)) {
            continue;
        }
        // The mesh asset is replaced through its existing handle, so nothing has to be respawned
        // and every entity pointing at it follows along.
        if let Some(mut slot) = meshes.get_mut(&handle.0) {
            *slot = ground_mesh(&ground.0, tile.tx, tile.tz);
        }
    }
    // The rock the same samples describe. It cannot go through its handle the way the ground does:
    // a stroke changes how many rocks a tile has, and a tile that ends up with none has no mesh to
    // write into at all. So the tile's rock is despawned and built again — the same shape as the
    // collider does it in `rebuild_patched_ground`, for the same reason.
    for (entity, tile) in rocks.iter() {
        if dirty.contains(&(tile.tx, tile.tz)) {
            commands.entity(entity).despawn();
        }
    }
    for &(tx, tz) in &dirty {
        if let Some(mesh) = rock_mesh(&ground.0, tx, tz) {
            commands.spawn(rock_tile(*root, RockTile { tx, tz }, meshes.add(mesh), material.clone()));
        }
    }
}

/// Update: the ground, as something to look at, in tiles.
///
/// Tiling is not tidiness. As one mesh the ground is half a million triangles with no way to leave
/// any of them out, and it costs the whole map whichever way you are facing: measured, 51 frames a
/// second became 20. A tile has an AABB, so the ones behind you are culled before they reach the
/// GPU. It is also the unit §6 of the plan wants for editing — a stroke dirties a tile and that
/// tile alone rebuilds.
///
/// Everything it built last time comes down first, because this runs again whenever the map
/// changes and a second map on top of the first is two grounds.
fn dress_the_ground(
    ground: Res<Ground>,
    root: Single<Entity, With<LevelRoot>>,
    old: Query<Entity, AnyDrawnTile>,
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut dressing: Dressing,
) {
    let Dressing { materials, assets, ours } = &mut dressing;
    for tile in old.iter() {
        commands.entity(tile).despawn();
    }

    let terrain = &ground.0;
    // The map's own rules, not this file's idea of them. A new map brings its own look and this
    // runs again when one arrives, so the material is written under one handle rather than added:
    // the tiles that are about to be spawned point at it either way.
    let material = crate::ground_material::dress(materials, assets, ours, &terrain.layers);
    let (wide, deep) = terrain.grid.tiles();
    for tz in 0..deep {
        for tx in 0..wide {
            commands.spawn((
                Name::from(format!("Ground {tx},{tz}")),
                Authored,
                GroundTile { tx, tz },
                Mesh3d(meshes.add(ground_mesh(terrain, tx, tz))),
                MeshMaterial3d(material.clone()),
                Transform::IDENTITY,
                ChildOf(*root),
            ));
            // The rock standing on it, when the samples describe any. Same material: the ground
            // shader is triplanar over world position and needs no uv set, so a boulder gets the
            // rock texture and the facets its own steepness earns, with nothing added here.
            if let Some(mesh) = rock_mesh(terrain, tx, tz) {
                commands
                    .spawn(rock_tile(*root, RockTile { tx, tz }, meshes.add(mesh), material.clone()));
            }
        }
    }
}

/// What a tile's rock is spawned as, in one place so that the first build and every rebuild agree.
fn rock_tile(
    root: Entity,
    tile: RockTile,
    mesh: Handle<Mesh>,
    material: Handle<crate::ground_material::GroundMaterial>,
) -> impl Bundle {
    (
        Name::from(format!("Rock {},{}", tile.tx, tile.tz)),
        Authored,
        tile,
        Mesh3d(mesh),
        MeshMaterial3d(material),
        Transform::IDENTITY,
        ChildOf(root),
    )
}

/// What it takes to put a material on the ground: the store to write it into, the server to load
/// its textures from, and the list of handles the mipmap pass works on.
#[derive(bevy::ecs::system::SystemParam)]
struct Dressing<'w> {
    materials: ResMut<'w, Assets<crate::ground_material::GroundMaterial>>,
    assets: Res<'w, AssetServer>,
    ours: ResMut<'w, crate::ground_material::GroundTextures>,
}

/// Everything one tile of the map draws: its ground and the rock standing on it.
///
/// Named because a map change has to take down both, and a query filter spelt out at the call site
/// is the kind of thing that gets one of the two added to it and not the other.
type AnyDrawnTile = Or<(With<GroundTile>, With<RockTile>)>;

/// One drawn tileful of rock, which a stroke finds the same way it finds the ground under it.
#[derive(Component)]
struct RockTile {
    tx: u32,
    tz: u32,
}

/// One drawn piece of ground: which tile it is, so a stroke can find it, and a marker so the next
/// map can take the last one's tiles down.
#[derive(Component)]
struct GroundTile {
    tx: u32,
    tz: u32,
}

/// Builds what the level looks like. What it collides as is
/// [`level::spawn_level`](noob_tube_shared::level::spawn_level), spawned alongside this.
///
/// Keeping the two separate is deliberate — real levels use a simplified collision mesh, and
/// building that split in now means no rework when actual geometry arrives.
/// PreUpdate: applies the placements the server has accepted.
///
/// Straight in, with no tick to wait for. Unlike a stroke, a marker has no collider and no per-tile
/// rebuild, so there is no rollback window for it to straddle — terrain.md §7 exempts placement
/// from §9's rule, and this is what that exemption looks like: three lines and no queue.
///
/// Nothing here judges the edit. Everything that could be refused was refused on the server, and a
/// second opinion on this side would be a second rule to keep in step with the first.
fn take_placements(
    mut inbox: Query<&mut MessageReceiver<MarkerChanged>>,
    mut ground: ResMut<Ground>,
) {
    for mut receiver in inbox.iter_mut() {
        for change in receiver.receive() {
            // Through `ResMut` on purpose: touching it is what tells `build_the_props` and
            // `draw_the_props` there is something to rebuild.
            ground.0.apply(&change);
        }
    }
}

/// PreUpdate: draws the props the map places, and takes down the last map's.
///
/// The picture's half of [`level::build_the_props`], registered on the same condition and for the
/// same reason. The two read the same marker list and the same ground, so the crate you can see
/// and the crate you collide with cannot drift apart — which is the property the old shared
/// `CRATES` constant bought, kept while the positions become content.
fn draw_the_props(
    ground: Res<Ground>,
    root: Single<Entity, With<PropsRoot>>,
    standing: Query<Entity, With<DrawnProp>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut commands: Commands,
) {
    for entity in standing.iter() {
        commands.entity(entity).despawn();
    }
    let crates: Vec<_> =
        ground.0.markers.iter().filter(|marker| marker.kind == level::CRATE).collect();
    if crates.is_empty() {
        return;
    }
    // One mesh and one material for the lot: they are the same box in the same colour, and a
    // handle each would be a copy each.
    let mesh = meshes.add(Cuboid::new(
        CRATE_HALF_EXTENT * 2.0,
        CRATE_HALF_EXTENT * 2.0,
        CRATE_HALF_EXTENT * 2.0,
    ));
    let material = materials.add(Color::srgb(0.55, 0.4, 0.3));
    for (index, marker) in crates.into_iter().enumerate() {
        commands.spawn((
            Name::from(format!("Crate {index}")),
            DrawnProp,
            Authored,
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            // The crate's own half-height, added here rather than stored in the marker — see
            // `level::default_markers`. The collider does the same, from the same numbers.
            Transform::from_translation(
                marker.where_it_stands(&ground.0) + Vec3::Y * CRATE_HALF_EXTENT,
            )
            .with_rotation(marker.rotation),
            ChildOf(*root),
        ));
    }
}

/// One drawn prop, so the next map's rebuild knows which meshes were the last map's.
#[derive(Component)]
struct DrawnProp;

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
    commands.spawn((
        Name::from("Props"),
        PropsRoot,
        Authored,
        Transform::IDENTITY,
        Visibility::default(),
        ChildOf(level),
    ));

    // The ground is not here. It is the map, and the map comes from the server — see
    // `adopt_the_map`, which hangs the tiles under this same root the moment it arrives.

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
        let terrain = noob_tube_shared::terrain::default_terrain();
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
                let (h00, h10) = (terrain.surface_at(x0, z0), terrain.surface_at(x1, z0));
                let (h01, h11) = (terrain.surface_at(x0, z1), terrain.surface_at(x1, z1));
                // Each cell is two triangles split along the anti-diagonal `u + v = 1`, which is
                // the edge `[here + 1, next_row]` above.
                let drawn = if u + v <= 1.0 {
                    h00 + (h10 - h00) * u + (h01 - h00) * v
                } else {
                    h11 + (h01 - h11) * (1.0 - u) + (h10 - h11) * (1.0 - v)
                };
                worst = worst.max((drawn - terrain.surface_at(ix, iz)).abs());
            }
        }
        assert!(worst < 0.7, "the drawn ground is {worst:.2} m from the ground underfoot");
    }
}
