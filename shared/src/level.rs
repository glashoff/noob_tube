//! The level's geometry, as both sides must see it.
//!
//! Client and server have to collide against exactly the same shapes. If they do not, the client's
//! prediction and the server's authority disagree about where a player can stand, and every step
//! near the difference produces a correction the player sees as a stutter. Keeping the numbers in
//! one place is the cheapest way to make that impossible.
//!
//! The visible meshes stay in the client, built from these same constants. That split is
//! deliberate — real levels use simplified collision geometry, and the two will diverge in shape
//! long before they diverge in intent.

use avian3d::prelude::{Collider, Position};
use bevy::prelude::*;

use crate::physics::{level_geometry, level_geometry_facing};
use crate::sculpt::GroundPatched;
use crate::terrain::{Ground, Marker, Terrain};

/// Half-extents of one crate. They are cubes, 2 m on a side.
pub const CRATE_HALF_EXTENT: f32 = 1.0;

/// Where the crates stand, at their centres. Something to bump into so collision and sliding are
/// visible at all.
pub const CRATES: [Vec3; 3] = [
    Vec3::new(6.0, 1.0, -8.0),
    Vec3::new(-5.0, 1.0, -12.0),
    Vec3::new(0.0, 1.0, -18.0),
];

/// A slope, so that there is something to drive up and roll back down.
///
/// Half-extents of the slab, before it is tilted. Wide enough for a vehicle and its mistakes.
pub const RAMP_HALF_EXTENTS: Vec3 = Vec3::new(3.0, 0.5, 6.0);
/// How steep it is, in radians — about 12°, which a vehicle climbs and a walking player does not
/// slide back down.
pub const RAMP_ANGLE: f32 = 0.21;
/// Where the middle of it stands, on the ground plane. Clear of the crates.
pub const RAMP_CENTRE: Vec2 = Vec2::new(14.0, -20.0);

/// Where the ramp sits and how it is turned.
///
/// Derived rather than written down, because the number that matters is not the slab's centre but
/// where its *driving surface* meets the ground: a ramp whose near edge is nine centimetres up is a
/// step, and a vehicle hits it rather than climbing it. Solving for that leaves the slab's lower
/// half buried, which is what a real ramp does too.
pub fn ramp_pose() -> (Vec3, Quat) {
    let (sin, cos) = RAMP_ANGLE.sin_cos();
    // The near top corner, in slab space, is (·, +y, +z); tilting takes it to
    // `y·cos − z·sin` below the centre, so the centre must stand exactly that high.
    let height = RAMP_HALF_EXTENTS.z * sin - RAMP_HALF_EXTENTS.y * cos;
    (
        Vec3::new(RAMP_CENTRE.x, height, RAMP_CENTRE.y),
        // Turning about +X lifts the −Z end, and −Z is forward everywhere in this game, so the ramp
        // climbs away from where players start.
        Quat::from_rotation_x(RAMP_ANGLE),
    )
}

/// Where the vehicles stand at the start of a round, on the ground plane, and which way each faces.
///
/// The first is in line with the ramp and a little way in front of it, so that driving straight
/// forward from a standing start arrives at the slope. The second stands across the map, turned a
/// quarter of a turn from the first — not decoration: two vehicles facing the same way tell you
/// nothing about whether the spawn honours a rotation, and one of them has to be somewhere a second
/// player can reach without walking past the first.
///
/// The angle is a yaw in radians, about +Y, applied to the −Z that is forward everywhere here.
pub const VEHICLE_STARTS: [(Vec2, f32); 2] = [
    (Vec2::new(14.0, -8.0), 0.0),
    (Vec2::new(-16.0, -4.0), core::f32::consts::FRAC_PI_2),
];

/// The placeables this build knows, which is the palette a map may name kinds from.
///
/// A `kind` is a palette id rather than an enum of the four that happen to exist, so adding a
/// placeable becomes adding an asset rather than a code change (terrain.md §7). Today the palette
/// is a constant here; when the server has more assets than the client, it is the server's to send.
pub const PLAYER_SPAWN: &str = "player";
pub const VEHICLE: &str = "vehicle";
pub const CRATE: &str = "crate";
/// A crate the solver moves, as against [`CRATE`], which is part of the level and never budges.
///
/// The two are one word apart on purpose: what a player needs to know about a box is whether it
/// will move, and everything else about it — its size, its weight, whether it is replicated at all
/// — follows from that one answer. A static crate is geometry both sides build from the marker; a
/// heavy one is a body the server owns and sends, which is why only the server spawns it.
pub const HEAVY_CRATE: &str = "heavy crate";
pub const PLACEABLES: [&str; 4] = [PLAYER_SPAWN, VEHICLE, CRATE, HEAVY_CRATE];

/// How many player spawns the built-in map lays out.
///
/// Eight rather than one per connected client, because a marker list is fixed content and the
/// player count is not. It is the same two-metre row [`spawn_point`] produced, and no better a
/// piece of level design — but it is now *editable*, which is the whole point of the change.
const DEFAULT_SPAWNS: usize = 8;

/// What stands on a map nobody has placed anything on.
///
/// The three constants above, as markers. This is the step where they stop being code: a map that
/// says nothing about what is on it gets this, and a map that says anything at all gets exactly
/// what it says. Nothing else in the game reads `CRATES` or `VEHICLE_STARTS` any more.
///
/// The heights are offsets above the ground, so each is the thing's own resting height rather than
/// a world y — a crate sits half its own height up, a vehicle and a player sit on the surface and
/// let their own spawn add whatever they stand on.
pub fn default_markers() -> Vec<Marker> {
    let mut markers = Vec::new();
    for index in 0..DEFAULT_SPAWNS {
        markers.push(Marker {
            id: 0,
            kind: PLAYER_SPAWN.into(),
            x: index as f32 * 2.0,
            z: 0.0,
            y: 0.0,
            rotation: Quat::IDENTITY,
        });
    }
    for (at, yaw) in VEHICLE_STARTS {
        markers.push(Marker {
            id: 0,
            kind: VEHICLE.into(),
            x: at.x,
            z: at.y,
            y: 0.0,
            rotation: Quat::from_rotation_y(yaw),
        });
    }
    for centre in CRATES {
        markers.push(Marker {
            id: 0,
            kind: CRATE.into(),
            x: centre.x,
            z: centre.z,
            // Zero, and the crate's own half-height is added where the crate is built. A marker
            // says where a thing goes and how far *above the ground* — how tall the thing that
            // lands there is, is the thing's business, the same split `spawn_vehicles` makes with
            // its ride height. Putting it in the marker would mean every placement had to know the
            // geometry of what it was placing, and a click would bury a crate to its middle.
            y: 0.0,
            rotation: Quat::IDENTITY,
        });
    }
    // Numbered in one place, at the end, rather than counted along the way: three loops each
    // maintaining an index is three chances for two markers to share a handle.
    for (index, marker) in markers.iter_mut().enumerate() {
        marker.id = index as u32;
    }
    markers
}

/// Where the nth player spawn of a map is, on the ground as it stands.
///
/// Reading the map rather than computing a row, which is the whole of what step eight changes here.
/// What it does **not** change is the choice of index: the server still passes a count of existing
/// players, so somebody joining after two others have left gets the third marker rather than the
/// first. That was wrong on a plane and is still wrong on a map — the fix is picking a *free*
/// spawn, which needs to know where everybody is standing and is a question about spawning rather
/// than about placement.
///
/// Wrapping, because the marker list is finite where the old row was not. Two players on one
/// marker is a worse outcome than two players two metres apart, and the answer to it is that the
/// spawns are now editable: a map that needs more says so.
///
/// A map with no player spawn at all should not exist — the server refuses a delete that would
/// make one — but a hand-written file can still be one, and the middle of the map is a better
/// answer to that than a panic.
pub fn nth_spawn(terrain: &Terrain, index: usize) -> Vec3 {
    let spawns: Vec<_> = terrain.markers_of(PLAYER_SPAWN).collect();
    if spawns.is_empty() {
        return Vec3::new(0.0, terrain.height_over(0.0, 0.0), 0.0);
    }
    spawns[index % spawns.len()].where_it_stands(terrain)
}

/// The bare position of the nth spawn, without a map to stand it on.
///
/// What remains of the old constant, and it exists for one caller: the level tests, which have no
/// terrain. Everything in the running game goes through [`nth_spawn`].
#[cfg(test)]
pub fn spawn_point(index: usize) -> Vec3 {
    Vec3::new(index as f32 * 2.0, 0.0, 0.0)
}

/// One tile of the ground's collider, and which tile it is.
///
/// Tiled rather than one shape, and the reason is sculpting: a stroke dirties a handful of samples,
/// and rebuilding a 513² height field for each of them would cost the whole map per brush tick. The
/// tiling is the map's own — [`Grid::TILE_CELLS`](crate::terrain::Grid::TILE_CELLS) — so a stroke
/// rebuilds the same tiles here and in the picture.
#[derive(Component)]
pub struct GroundCollider {
    pub tx: u32,
    pub tz: u32,
}

/// PreUpdate: builds the ground the current map describes, and takes down whatever was there.
///
/// Run whenever [`Ground`] appears or changes, rather than at startup, because on a client it does
/// not exist at startup — the map arrives from the server over its own channel (terrain.md §3), and
/// until it has, this client has no ground and knows it.
///
/// PreUpdate rather than Update: `FixedMain` runs before `Update` inside a frame, so a collider
/// built in `Update` would be one tick of physics late, and the first tick of that frame would find
/// no ground at all.
///
/// The point of it being this short is that nothing downstream had to change. `level_geometry`
/// gives it the static body and the level layer that `Level`'s sweeps and rays need, and every
/// query beyond that goes through Avian.
pub fn build_the_ground(
    ground: Res<Ground>,
    old: Query<Entity, With<GroundCollider>>,
    mut commands: Commands,
) {
    for previous in old.iter() {
        commands.entity(previous).despawn();
    }
    let (wide, deep) = ground.0.grid.tiles();
    for tz in 0..deep {
        for tx in 0..wide {
            let (collider, at) = ground.0.tile_collider(tx, tz);
            commands.spawn((GroundCollider { tx, tz }, level_geometry(collider, at)));
        }
    }
}

/// PreUpdate: rebuilds the tiles a stroke moved, and only those.
///
/// The collider is replaced in place rather than the entity respawned, which is what makes this
/// idempotent — and it has to be, because a rollback can re-run the frame that triggers it. Running
/// it twice on the same patch writes the same shape twice.
///
/// A tile is a *window* of the samples and neighbouring tiles share their edges, so a patch that
/// ends exactly on a boundary dirties the tile on each side of it. Rebuilding one tile too many is
/// invisible; one too few leaves a seam of old ground standing.
pub fn rebuild_patched_ground(
    ground: Res<Ground>,
    mut patched: MessageReader<GroundPatched>,
    tiles: Query<(Entity, &GroundCollider)>,
    mut commands: Commands,
) {
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
    for (entity, tile) in tiles.iter() {
        if dirty.contains(&(tile.tx, tile.tz)) {
            let (collider, at) = ground.0.tile_collider(tile.tx, tile.tz);
            commands.entity(entity).insert((collider, Position(at)));
        }
    }
}

/// Startup: builds the one thing that is level rather than map — the ramp.
///
/// A static rigid body on the level layer, identical on both sides by construction: it is a
/// constant, and both binaries read the same one.
///
/// The ground is deliberately not here, and neither are the crates any more. Both are the map,
/// which the server owns and sends — see [`build_the_ground`] and [`build_the_props`].
pub fn spawn_level(mut commands: Commands) {
    let (at, facing) = ramp_pose();
    commands.spawn(level_geometry_facing(
        Collider::cuboid(
            RAMP_HALF_EXTENTS.x * 2.0,
            RAMP_HALF_EXTENTS.y * 2.0,
            RAMP_HALF_EXTENTS.z * 2.0,
        ),
        at,
        facing,
    ));
}

/// One crate the map placed, so a map switch knows which bodies were the last map's.
#[derive(Component)]
pub struct PlacedProp;

/// PreUpdate: builds the solid things the map places, and takes down whatever the last map placed.
///
/// Beside [`build_the_ground`] and run on the same condition, because it answers to the same
/// thing: the marker list arrives with the map, so a client has none of this at startup and both
/// sides get it from the copy the server sent.
///
/// Rebuilt wholesale rather than diffed. The list is a few dozen entries and a map switch is not a
/// per-tick event; a diff would be a second description of what changed, kept in step with the
/// first by hand.
///
/// It reads the ground under each marker rather than a stored height, which is the point of the
/// offset being an offset: sculpt under a crate and it comes up with the hill on the next rebuild
/// rather than hanging in the air.
pub fn build_the_props(
    ground: Res<Ground>,
    standing: Query<Entity, With<PlacedProp>>,
    mut commands: Commands,
) {
    for entity in standing.iter() {
        commands.entity(entity).despawn();
    }
    for marker in &ground.0.markers {
        if marker.kind != CRATE {
            continue;
        }
        // Avian sizes a cuboid by its full side lengths, where rapier takes half-extents.
        commands.spawn((
            level_geometry_facing(
                Collider::cuboid(
                    CRATE_HALF_EXTENT * 2.0,
                    CRATE_HALF_EXTENT * 2.0,
                    CRATE_HALF_EXTENT * 2.0,
                ),
                marker.where_it_stands(&ground.0) + Vec3::Y * CRATE_HALF_EXTENT,
                marker.rotation,
            ),
            PlacedProp,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::movement::WALKABLE_NORMAL_Y;
    use crate::physics::test_support::{ask, bare_app};

    /// The level a round is actually played on, built by the systems that build it for real.
    ///
    /// Both of them, and that is the point: the built geometry is a constant either side can spawn,
    /// and the ground is a map that has to be *given* to the world first. A test that only ran
    /// `spawn_level` would be testing a world with no ground in it, which is exactly what a client
    /// has before the server's map arrives.
    ///
    /// They are run directly rather than added to `Startup`: the harness hands back an app that has
    /// already started, so a system added afterwards never runs and every probe would come back
    /// empty — a test that passes for the wrong reason on the day the ground disappears.
    fn played_level() -> App {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = bare_app();
        app.insert_resource(crate::terrain::Ground(crate::terrain::default_terrain()));
        app.world_mut().run_system_once(spawn_level).expect("the level spawns");
        app.world_mut().run_system_once(build_the_ground).expect("the ground is built");
        app.world_mut().run_system_once(build_the_props).expect("the props are built");
        app.update();
        app
    }

    /// Straight down from overhead, and the world y it landed on.
    fn ground_at(app: &mut App, x: f32, z: f32) -> Option<f32> {
        ask(app, move |level| level.raycast(Vec3::new(x, 500.0, z), Vec3::NEG_Y, 1000.0))
            .map(|distance| 500.0 - distance)
    }

    /// The ground is a height field now, and everything that stood on the plane still does.
    ///
    /// This is the whole claim of the swap: `Level`'s rays and sweeps go through Avian, so a
    /// height field is ground in exactly the way a triangle mesh was, without a line of change
    /// anywhere downstream. If that were not true it would show here first.
    #[test]
    fn the_ground_is_still_under_everything_that_starts_on_it() {
        let mut app = played_level();
        for (at, _) in VEHICLE_STARTS {
            let y = ground_at(&mut app, at.x, at.y).expect("no ground under a vehicle start");
            assert!(y.abs() < 0.01, "the ground under {at:?} is at {y:.3}, not at the plane");
        }
        for index in 0..4 {
            let at = spawn_point(index);
            let y = ground_at(&mut app, at.x, at.z).expect("no ground under a spawn point");
            assert!(y.abs() < 0.01, "the ground under spawn {index} is at {y:.3}");
        }
        // And a player stands on it rather than through it.
        assert!(ask(&mut app, |level| level.is_grounded(Vec3::ZERO, false)), "not standing on it");
    }

    /// The field reaches as far as the plane did, and stops where it says it stops.
    #[test]
    fn the_ground_covers_the_playable_area() {
        let mut app = played_level();
        let half = crate::terrain::DEFAULT_EXTENT / 2.0;
        assert!(ground_at(&mut app, half - 1.0, half - 1.0).is_some(), "a corner is missing");
        assert!(ground_at(&mut app, -half + 1.0, -half + 1.0).is_some(), "a corner is missing");
        assert!(
            ground_at(&mut app, half + 5.0, 0.0).is_none(),
            "there is ground past the edge of the map",
        );
    }

    /// The ramp and the crates are still there and still on top of the ground.
    ///
    /// The crates come off the map's marker list now rather than out of a constant, so this says
    /// two things at once: the height field is still under them, and the default markers put them
    /// exactly where the constant did. The second is what makes the change to markers invisible to
    /// anybody playing.
    #[test]
    fn the_scenery_still_sits_on_the_ground() {
        let mut app = played_level();
        let ramp = ground_at(&mut app, RAMP_CENTRE.x, RAMP_CENTRE.y).expect("no ramp");
        assert!(ramp > 0.5, "the ramp reads as {ramp:.2} m up, which is the ground, not the ramp");
        for centre in CRATES {
            let top = ground_at(&mut app, centre.x, centre.z).expect("no crate");
            let want = centre.y + CRATE_HALF_EXTENT;
            assert!((top - want).abs() < 0.01, "a crate top is at {top:.2}, not {want:.2}");
        }
    }

    /// The default markers are the three constants, and nothing has moved.
    ///
    /// The constants are still here, and this is why: they are what says the map somebody starts on
    /// today is the map they started on yesterday. When a real map replaces them they go, and this
    /// test goes with them.
    #[test]
    fn the_default_markers_put_everything_where_the_constants_did() {
        let terrain = crate::terrain::default_terrain();
        let markers = &terrain.markers;
        assert_eq!(markers.len(), DEFAULT_SPAWNS + VEHICLE_STARTS.len() + CRATES.len());

        for (marker, (at, yaw)) in terrain.markers_of(VEHICLE).zip(VEHICLE_STARTS) {
            assert_eq!((marker.x, marker.z), (at.x, at.y));
            assert!(marker.rotation.dot(Quat::from_rotation_y(yaw)).abs() > 1.0 - 1.0e-6);
        }
        for (marker, centre) in terrain.markers_of(CRATE).zip(CRATES) {
            assert_eq!((marker.x, marker.z), (centre.x, centre.z));
            // The constant is the *centre* of a box on a plane at zero; the marker is the point on
            // the ground it stands on, and the box's own half-height is added where the box is
            // built. The two agree once that is put back, and that agreement is what is pinned.
            let stands = marker.where_it_stands(&terrain).y + CRATE_HALF_EXTENT;
            assert!(
                (stands - centre.y).abs() < 0.01,
                "a crate marker puts its box at {stands} where the constant says {}",
                centre.y,
            );
        }
        for index in 0..DEFAULT_SPAWNS {
            let want = spawn_point(index);
            let got = nth_spawn(&terrain, index);
            assert!((got.x - want.x).abs() < 0.01 && (got.z - want.z).abs() < 0.01);
        }
    }

    /// A map with no player spawn does not panic; it puts people in the middle of it.
    ///
    /// A hand-written manifest can be one, and the server refuses a delete that would make one, so
    /// this is the case nobody reaches on purpose and everybody would rather not crash on.
    #[test]
    fn a_map_with_nowhere_to_spawn_still_answers() {
        let mut terrain = crate::terrain::default_terrain();
        terrain.markers.retain(|marker| marker.kind != PLAYER_SPAWN);
        let at = nth_spawn(&terrain, 3);
        assert_eq!((at.x, at.z), (0.0, 0.0));
    }

    /// A map arriving takes the place of the map before it, rather than joining it.
    ///
    /// Two grounds is worse than none: the rays would find whichever was higher and players would
    /// stand on a surface nobody can see. This is the property step six needs — load a map
    /// mid-round and the old one goes — and it is one line of the system, so it is worth a test
    /// before there is a second map to find it with.
    #[test]
    fn a_second_map_replaces_the_first() {
        use bevy::ecs::system::RunSystemOnce;
        let mut app = played_level();
        app.world_mut().run_system_once(build_the_ground).expect("the ground is rebuilt");
        app.update();
        let grounds = app
            .world_mut()
            .query_filtered::<Entity, With<GroundCollider>>()
            .iter(app.world())
            .count();
        let (wide, deep) = crate::terrain::default_terrain().grid.tiles();
        let want = (wide * deep) as usize;
        assert_eq!(grounds, want, "{grounds} ground tiles after building {want} of them twice");
    }

    /// A stroke has to reach the ground a player stands on, not only the numbers behind it.
    ///
    /// The tile that changed has its `Collider` replaced in place, and whether Avian notices a
    /// swapped component is exactly the kind of thing to find out by asking rather than by reading.
    /// So: sculpt the field, rebuild the tiles the patch touched, and cast the ray the movement
    /// code casts.
    ///
    /// It rebuilds twice, because a rollback can re-run the frame that triggers this and the
    /// second run must leave the same shape rather than a second one.
    #[test]
    fn a_stroke_moves_the_ground_a_player_stands_on() {
        use bevy::ecs::system::RunSystemOnce;
        use crate::sculpt::{Brush, Stroke};

        let mut app = played_level();
        app.add_message::<GroundPatched>();
        let before = ground_at(&mut app, 0.0, 0.0).expect("ground at the origin");
        let away_before = ground_at(&mut app, 100.0, 100.0).expect("ground away from the stroke");

        let stroke = Stroke { at: Vec2::ZERO, radius: 12.0, brush: Brush::Lift { metres: 5.0 } };
        let patch = {
            let mut ground = app.world_mut().resource_mut::<Ground>();
            ground.0.sculpt(&stroke).expect("the stroke wrote something")
        };
        for _ in 0..2 {
            app.world_mut().write_message(GroundPatched(patch));
            app.world_mut().run_system_once(rebuild_patched_ground).expect("the rebuild");
            app.update();
        }

        let after = ground_at(&mut app, 0.0, 0.0).expect("ground at the origin");
        assert!(
            (after - before - 5.0).abs() < 0.05,
            "the ground went from {before:.2} to {after:.2}, which is not five metres",
        );
        // And the ground well outside the brush did not move with it.
        //
        // Against what the ray found *before* the stroke rather than against the height field,
        // because the two are not the same thing any more: (100, 100) is a wall of the bowl, and
        // what a ray finds on a wall is the rock standing on it — a metre and a half above the
        // samples, which is exactly what `rock` is for. The claim here was never about the height
        // field, it was that a stroke reaches this far and no further.
        let away = ground_at(&mut app, 100.0, 100.0).expect("ground away from the stroke");
        assert!(
            (away - away_before).abs() < 0.05,
            "ground 140 m away went from {away_before:.2} to {away:.2}",
        );
    }

    /// A hillside of the real map is ground a player stands on, and it faces the way it looks.
    ///
    /// Worth its own test because the slope limit is only as good as the normal it reads, and that
    /// normal comes from parry's height field rather than from anything written here. A field that
    /// handed back a downward normal, or a constant one, would pass every test above and turn every
    /// hill on the map into a cliff.
    #[test]
    fn a_hillside_of_the_real_map_is_stood_on() {
        let mut app = played_level();
        // On the flank of the hill at (-140, 60), well outside the flat the level stands on. The
        // long ray finds it; the short one is the probe the movement code actually uses.
        let feet = ground_at(&mut app, -85.0, 60.0).expect("no hillside there") + 0.05;
        let (ground, normal) = ask(&mut app, move |level| {
            level.ground_below(Vec3::new(-85.0, feet, 60.0))
        })
        .expect("the ground probe lost a hillside a long ray found");
        assert!(ground > 1.0, "the hill flank is at {ground:.2} m, which is the plane");
        assert!(normal.y > WALKABLE_NORMAL_Y, "the hill faces {normal:?}, which is a cliff");
        assert!(normal.y < 0.999, "the hill flank is level, so nothing about slope was tested");
        assert!(
            ask(&mut app, move |level| level.is_grounded(Vec3::new(-85.0, feet, 60.0), false)),
            "a player on the hillside is falling",
        );
    }

    /// Two vehicles that start inside each other, or inside a crate, spend the first tick of the
    /// round being pushed apart — which looks like a bug and is one. Nothing checks this at spawn,
    /// so it is checked here, where adding a third vehicle will trip over it.
    #[test]
    fn the_vehicles_start_clear_of_each_other_and_of_the_scenery() {
        // The chassis is 1.8 x 3.8 m; its longest half-diagonal is what has to clear anything else,
        // whichever way round it is turned.
        let reach = crate::vehicle::BUGGY.half_extents.xz().length();
        for (index, (at, _)) in VEHICLE_STARTS.iter().enumerate() {
            for (other, _) in VEHICLE_STARTS.iter().skip(index + 1) {
                let gap = at.distance(*other);
                assert!(gap > reach * 2.0, "two vehicles start {gap:.1} m apart");
            }
            for centre in CRATES {
                let gap = at.distance(centre.xz());
                let clearance = reach + CRATE_HALF_EXTENT * 2f32.sqrt();
                assert!(gap > clearance, "a vehicle starts {gap:.1} m from a crate");
            }
            let (ramp, _) = ramp_pose();
            let gap = at.distance(ramp.xz());
            assert!(gap > reach + RAMP_HALF_EXTENTS.xz().length(), "a vehicle starts on the ramp");
        }
    }

    /// The whole point of deriving the pose: a vehicle must be able to drive on to the ramp, not
    /// into it. The near edge of the driving surface has to be level with the ground.
    #[test]
    fn the_ramp_meets_the_ground_at_its_near_edge() {
        let (at, facing) = ramp_pose();
        let near_top = at + facing * Vec3::new(0.0, RAMP_HALF_EXTENTS.y, RAMP_HALF_EXTENTS.z);
        assert!(near_top.y.abs() < 1e-5, "the ramp starts {:.3} m off the ground", near_top.y);
    }

    /// And it has to actually go somewhere, or it is a plate on the floor.
    #[test]
    fn the_ramp_climbs() {
        let (at, facing) = ramp_pose();
        let far_top = at + facing * Vec3::new(0.0, RAMP_HALF_EXTENTS.y, -RAMP_HALF_EXTENTS.z);
        assert!(far_top.y > 2.0, "the ramp only reaches {:.2} m", far_top.y);
    }
}
