//! Maps as files, and the requests that make, load and save them.
//!
//! Terrain is the first thing in this project that is *content* rather than constants. `level.rs`
//! is compiled in; a height field is a file, and the whole point of sculpting is that it changes —
//! so terrain arrives together with the map management the game did not have, because a sculpt you
//! cannot save is a demo.
//!
//! **Any connected player may create a map, load one, and save changes.** No ownership and no
//! permissions, which is the stance the rest of the game takes. What is guarded is size and
//! accident, not the player:
//!
//! - **Extent and spacing are clamped**, in `shared`, so the menu refuses an over-cap map against
//!   the same constants the server enforces. Unclamped they are a denial of service with a one-line
//!   exploit: a 10 km map at 5 cm spacing is 40 000² samples and 3.2 GB, allocated on the server and
//!   sent to everybody who joins.
//! - **Creating is rate-limited per client**, because it is the action that leaves a file behind.
//! - **A name is sanitised on arrival**, and a *load resolves a name against the list of maps the
//!   server itself found*. That is the path-traversal guard, and it is deliberately not a separate
//!   check standing beside the load — it is how the load works, which is what stops it being
//!   forgotten on the second code path.
//!
//! Saving is explicit and never automatic. The file on disk is what the next joiner starts from,
//! and an accidental save over a good map with a half-finished experiment is unrecoverable without
//! versioning this project does not have.

use avian3d::prelude::{AngularVelocity, LinearVelocity, Position, Rotation};
use bevy::prelude::*;
use lightyear::prelude::*;
use noob_tube_shared::level::{self, VEHICLE_STARTS};
use noob_tube_shared::player::PlayerState;
use noob_tube_shared::protocol::TerrainChannel;
use noob_tube_shared::terrain::{
    CREATE_INTERVAL, Ground, MapFault, MapList, MapRequest, Manifest, Terrain,
    TerrainBaseline, VERSION, sanitise_name,
};
use noob_tube_shared::vehicle::{VehicleKind, Wheels};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where maps live.
///
/// Anchored to the source tree rather than to the working directory, for the same reason the
/// client's asset root is: a cargo build runs from wherever it likes, and a map written beside the
/// binary lands in `target/debug`, which `cargo clean` deletes. Shipping a server means making this
/// a setting, which is a packaging question and is not one yet.
pub const MAPS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../maps");

/// What the server knows about maps, beside the one being played.
///
/// The list is *cached*, and that is not an optimisation. It is the set a load may resolve a name
/// against, so it has one owner and one place it is refreshed — a load that re-scanned the
/// directory for itself would be a second code path with its own chance of accepting a name that
/// is not on it.
#[derive(Resource)]
pub struct Maps {
    pub dir: PathBuf,
    pub names: Vec<String>,
    /// The map being played, or `None` for the built-in one, which has no file behind it.
    pub current: Option<String>,
    /// Whether the map in play differs from the file it came from. Nothing sets it until sculpting.
    pub unsaved: bool,
    /// When each peer last created a map, for the rate limit.
    last_create: HashMap<PeerId, std::time::Instant>,
}

impl Maps {
    /// Reads whatever is on disk. A directory that is not there yet is an empty list, not an error:
    /// a fresh checkout has no maps and that is a perfectly good state to start a server in.
    pub fn discover(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let names = scan(&dir);
        Self { dir, names, current: None, unsaved: false, last_create: HashMap::new() }
    }

    /// What goes back to a client after anything happens.
    fn report(&self, trouble: Option<MapFault>) -> MapList {
        MapList {
            maps: self.names.clone(),
            current: self.current.clone(),
            unsaved: self.unsaved,
            trouble: trouble.map(|fault| fault.to_string()),
        }
    }
}

/// Every map in the directory, sorted, by the name in front of the manifest.
///
/// Only names with a manifest count. A stray `.heights` with no `.json` beside it is not half a
/// map, it is not a map, and it must not appear in a list that a load resolves against.
fn scan(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != "json" {
                return None;
            }
            let stem = path.file_stem()?.to_str()?;
            // Round-tripped through the same rule a client's name goes through, so a file dropped
            // in by hand cannot put a name in the list that could not have been made here.
            sanitise_name(stem).ok().filter(|clean| clean == stem)
        })
        .collect();
    names.sort();
    names
}

fn manifest_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

fn heights_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.heights"))
}

/// Reads a map. The pair is never trusted: the manifest says how many samples there are and the
/// blob is however many bytes it is, and [`Terrain::decode`] is where the two are made to agree.
fn read(dir: &Path, name: &str) -> Result<Terrain, MapFault> {
    let text = std::fs::read_to_string(manifest_path(dir, name)).map_err(|error| {
        warn!("cannot read the manifest of {name}: {error}");
        MapFault::NoSuchMap
    })?;
    let manifest: Manifest = serde_json::from_str(&text).map_err(|error| {
        warn!("the manifest of {name} is not one this build reads: {error}");
        MapFault::Version(0)
    })?;
    manifest.check()?;
    let blob = std::fs::read(heights_path(dir, name)).map_err(|error| {
        warn!("cannot read the heights of {name}: {error}");
        MapFault::NoSuchMap
    })?;
    Terrain::decode(manifest.grid, manifest.water_y, &blob)
}

/// Writes one, manifest first and heights second.
///
/// That order matters on a crash: a manifest with no heights beside it is not listed by [`scan`],
/// so the half-written map is invisible rather than loadable-and-wrong.
fn write(dir: &Path, name: &str, terrain: &Terrain) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let manifest = Manifest {
        version: VERSION,
        grid: terrain.grid,
        water_y: terrain.water_y,
        layers: Vec::new(),
        markers: Vec::new(),
    };
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    std::fs::write(manifest_path(dir, name), text)?;
    std::fs::write(heights_path(dir, name), terrain.encode())
}

/// What one request needs from the world, beside the maps themselves.
type Switching<'w, 's> = (
    ResMut<'w, Ground>,
    Query<'w, 's, &'static mut PlayerState>,
    Query<'w, 's, (&'static VehicleKind, &'static mut Position, &'static mut Rotation, &'static mut LinearVelocity, &'static mut AngularVelocity, &'static mut Wheels)>,
);

/// PreUpdate: does what clients have asked of the maps, and tells them what came of it.
///
/// Before `FixedMain`, so a map that changes this frame is the map this frame's ticks are simulated
/// against, and before [`level::build_the_ground`](noob_tube_shared::level::build_the_ground),
/// which watches the resource this writes.
///
/// Every path answers, including the ones that fail. A menu that asked for something and heard
/// nothing back cannot tell a refusal from a lost packet, and this channel is reliable precisely so
/// that it never has to.
pub fn serve_map_requests(
    mut inbox: Query<(&RemoteId, &mut MessageReceiver<MapRequest>)>,
    mut maps: ResMut<Maps>,
    mut world: Switching,
    mut sender: ServerMultiMessageSender,
    server: Single<&Server>,
) {
    let mut asked: Vec<(PeerId, MapRequest)> = Vec::new();
    for (remote, mut receiver) in inbox.iter_mut() {
        for request in receiver.receive() {
            asked.push((remote.0, request));
        }
    }

    for (peer, request) in asked {
        let switched = matches!(request, MapRequest::Create { .. } | MapRequest::Load { .. });
        let trouble = act(&mut maps, &mut world, peer, request).err();
        if trouble.is_none() && switched {
            place_everything(&mut world);
        }

        let report = maps.report(trouble);
        if let Err(error) =
            sender.send::<_, TerrainChannel>(&report, *server, &NetworkTarget::Single(peer))
        {
            warn!("could not answer {peer:?} about maps: {error}");
        }
        // A switch is everybody's business, and the baseline goes out on the same ordered channel
        // right behind the list — so a client cannot apply the new map's name to the old map's
        // ground.
        if trouble.is_none() && switched {
            let baseline = TerrainBaseline::of(&world.0.0);
            if let Err(error) =
                sender.send::<_, TerrainChannel>(&baseline, *server, &NetworkTarget::All)
            {
                error!("could not send the new map: {error}");
            }
        }
    }
}

/// One request, and the only place any of them changes anything.
fn act(
    maps: &mut Maps,
    world: &mut Switching,
    peer: PeerId,
    request: MapRequest,
) -> Result<(), MapFault> {
    match request {
        MapRequest::List => Ok(()),

        MapRequest::Create { name, extent_x, extent_z, spacing, min_y, max_y } => {
            let name = sanitise_name(&name)?;
            if maps.names.contains(&name) {
                return Err(MapFault::NameTaken);
            }
            // The rate limit is on this alone: creating leaves a file behind, and the other two do
            // not leave anything a restart would still be carrying.
            let now = std::time::Instant::now();
            if let Some(last) = maps.last_create.get(&peer)
                && now.duration_since(*last) < CREATE_INTERVAL
            {
                return Err(MapFault::TooFast);
            }
            // `Terrain::new` is where the caps are: a map that fails them is never allocated.
            let terrain = Terrain::new(extent_x, extent_z, spacing, min_y, max_y)?;
            write(&maps.dir, &name, &terrain).map_err(|error| {
                error!("could not write the map {name}: {error}");
                MapFault::NoSuchMap
            })?;
            maps.last_create.insert(peer, now);
            maps.names.push(name.clone());
            maps.names.sort();
            maps.current = Some(name.clone());
            maps.unsaved = false;
            world.0.0 = terrain;
            info!("{peer:?} made the map {name}");
            Ok(())
        }

        MapRequest::Load { name } => {
            // Resolved against the list, which is the whole of the traversal guard: a name that is
            // not one the server found is not a name at all, whatever it looks like.
            let name = maps
                .names
                .iter()
                .find(|known| *known == &name)
                .cloned()
                .ok_or(MapFault::NoSuchMap)?;
            let terrain = read(&maps.dir, &name)?;
            maps.current = Some(name.clone());
            maps.unsaved = false;
            world.0.0 = terrain;
            info!("{peer:?} loaded the map {name}");
            Ok(())
        }

        MapRequest::Save { name } => {
            let name = sanitise_name(&name)?;
            write(&maps.dir, &name, &world.0.0).map_err(|error| {
                error!("could not write the map {name}: {error}");
                MapFault::NoSuchMap
            })?;
            if !maps.names.contains(&name) {
                maps.names.push(name.clone());
                maps.names.sort();
            }
            maps.current = Some(name.clone());
            maps.unsaved = false;
            info!("{peer:?} saved the map {name}");
            Ok(())
        }
    }
}

/// Puts everybody back on the ground after a map switch.
///
/// A new map is flat at the middle of its own range, which is only y = 0 if the author chose a
/// symmetric one — so the round starts again rather than leaving players and vehicles hanging in
/// the air or buried. It reads the heights directly rather than casting rays, because the collider
/// for the new map does not exist yet: it is built from the resource this has just written, one
/// system later.
///
/// The ramp and the crates are left where they are, and that is the rule rather than an oversight.
/// Terrain is the ground; everything built is somebody else's geometry, and neither system asks the
/// other what it contains. Markers, in step eight, are what will let them move with the map.
fn place_everything(world: &mut Switching) {
    let ground = world.0.0.clone();
    for (index, mut state) in world.1.iter_mut().enumerate() {
        let at = level::spawn_point(index);
        *state = PlayerState {
            position: Vec3::new(at.x, ground.height_over(at.x, at.z), at.z),
            ..PlayerState::default()
        };
    }
    for (index, (kind, mut position, mut rotation, mut velocity, mut spin, mut wheels)) in
        world.2.iter_mut().enumerate()
    {
        let (at, yaw) = VEHICLE_STARTS[index % VEHICLE_STARTS.len()];
        position.0 = Vec3::new(
            at.x,
            ground.height_over(at.x, at.y) + kind.spec().ride_height(),
            at.y,
        );
        rotation.0 = Quat::from_rotation_y(yaw);
        velocity.0 = Vec3::ZERO;
        spin.0 = Vec3::ZERO;
        *wheels = Wheels::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of its own per test, so two of them running at once cannot see each other's
    /// maps. Cleaned up by the OS, not by us: a test that failed is one whose files are worth
    /// looking at.
    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "noob-tube-maps-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    /// A map written and read back is the map that was written. The whole of "it survives a
    /// restart" rests on this, and on nothing else.
    #[test]
    fn a_map_survives_being_written_and_read() {
        let dir = scratch("roundtrip");
        let made = Terrain::new(128.0, 96.0, 2.0, -20.0, 40.0).expect("a map within the caps");
        write(&dir, "a map", &made).expect("it writes");
        let back = read(&dir, "a map").expect("it reads");
        assert_eq!(back, made, "the map changed on the way through the disk");
        assert_eq!(scan(&dir), vec!["a map".to_string()], "it is not in the list");
    }

    /// Half a map is not a map. A manifest with no heights beside it — a crash between the two
    /// writes — must not be listed, because the list is what a load resolves a name against.
    #[test]
    fn a_map_with_no_heights_is_not_listed_and_does_not_load() {
        let dir = scratch("halfwritten");
        let made = Terrain::new(64.0, 64.0, 1.0, -10.0, 10.0).expect("a map within the caps");
        write(&dir, "whole", &made).expect("it writes");
        std::fs::remove_file(heights_path(&dir, "whole")).expect("the heights go");
        assert!(read(&dir, "whole").is_err(), "half a map loaded");
    }

    /// And a blob that does not match its manifest is refused rather than read off its own end.
    #[test]
    fn a_map_whose_two_files_disagree_is_refused() {
        let dir = scratch("mismatch");
        let made = Terrain::new(64.0, 64.0, 1.0, -10.0, 10.0).expect("a map within the caps");
        write(&dir, "trimmed", &made).expect("it writes");
        let mut blob = std::fs::read(heights_path(&dir, "trimmed")).expect("the heights");
        blob.truncate(blob.len() - 2);
        std::fs::write(heights_path(&dir, "trimmed"), blob).expect("the shorter heights");
        assert!(
            matches!(read(&dir, "trimmed"), Err(MapFault::BlobSize { .. })),
            "a map read off the end of itself",
        );
    }

    /// The scan only reports names that could have been made here, so a file dropped into the
    /// directory by hand cannot put something in the list that a load would then accept.
    #[test]
    fn a_file_that_is_not_a_map_is_not_in_the_list() {
        let dir = scratch("strays");
        let made = Terrain::new(64.0, 64.0, 1.0, -10.0, 10.0).expect("a map within the caps");
        write(&dir, "real", &made).expect("it writes");
        std::fs::write(dir.join("notes.txt"), "not a map").expect("a stray");
        std::fs::write(dir.join("../escape.json"), "{}").ok();
        std::fs::write(dir.join("half.heights"), [0u8; 8]).expect("a stray blob");
        std::fs::write(dir.join("../..%2fetc.json"), "{}").ok();
        assert_eq!(scan(&dir), vec!["real".to_string()]);
    }
}
