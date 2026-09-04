//! Turns a heightmap image into a map the server can load — terrain.md §14's *importing a height
//! field*, as a tool rather than a runtime feature.
//!
//!     cargo run --release -p import_heightmap -- ~/Downloads/Terrain004_8K.exr
//!     cargo run --release -p import_heightmap -- terrain.exr --name "north ridge" --relief 80
//!     cargo run --release -p import_heightmap -- terrain.exr --samples 513 --dry-run
//!
//! It writes `maps/<name>.heights` and `maps/<name>.json`, which is the pair `server/src/maps.rs`
//! reads back. **Both files are written through `shared`'s own types** — `Grid::quantise`,
//! `Terrain::encode` and `Manifest` — rather than through a second description of the format here,
//! so a map this writes is one the server can open by construction, and the day the format moves
//! this moves with it or stops compiling.
//!
//! Why it is a tool and not a button: an import overwrites *every* height on a map at once, which
//! is the one edit the game has no undo for. terrain.md says the same, and adds the other half of
//! the reason — a heightmap is a file somebody already has, on a disk the server cannot see.
//!
//! ## What it decides, and why those are the decisions
//!
//! A heightmap is a picture of relative heights: it has no metres in it, no footprint, and nothing
//! that says where anybody comes in. Three things have to be supplied on the way through, and each
//! is an option with a default that this file argues for:
//!
//! - **The footprint** — `--samples` and `--spacing`. Heights are grid *points*, so an n-metre map
//!   at 1 m spacing wants n+1 of them, which is why heightmap tools export 513, 1025 and 2049 and
//!   why this defaults to 1025: a 1024 m map at 1 m spacing, the largest canonical square that
//!   fits inside the 4 MiB baseline the whole map travels to every client as.
//! - **The relief** — `--relief`, peak-to-trough in metres, centred on y = 0. The source's own
//!   range is mapped onto it linearly, so the shape is the file's and the scale is the author's.
//!   It is *not* the full `--min-y`..`--max-y` range on purpose: a field pressed against both ends
//!   of its quantisation range cannot be sculpted upward at the summit or downward in the valley
//!   without clamping, and the range cannot be changed afterwards without requantising the map.
//! - **Where the markers go** — searched for, not assumed. `level::default_markers` puts eight
//!   spawns, two vehicles and three crates in a thirty-metre huddle around the world origin, which
//!   on generated terrain is as likely to be a cliff face as anything else. So the flattest place
//!   that will hold the huddle is found first and the whole set is moved there in one piece,
//!   keeping the layout somebody designed and only choosing the ground under it.
//!
//! Everything the run decided is printed, including a slope profile of the finished map, because
//! `--relief` is the number an author will want to change and the profile is what tells them which
//! way. Nothing about the source file is guessed at silently.

use std::path::{Path, PathBuf};

use noob_tube_shared::level::default_markers;
use noob_tube_shared::terrain::{
    DEFAULT_MAX_Y, DEFAULT_MIN_Y, DEFAULT_SPACING, Grid, Manifest, Marker, Terrain, VERSION,
    default_layers, sanitise_name, slope_degrees, surface_of, waterline,
};

/// Samples per axis when nobody says otherwise, and the reason the cap is 2049 and not 2048.
///
/// 1025 is a 1024 m map at 1 m spacing and 2.1 MB of heights — half of `MAX_BASELINE_BYTES`, so
/// there is room for a rectangle that is longer on one axis. The next size up, 2049², is 8.4 MB
/// and is refused by the total; the size down, 513², is what every map in this repository has been
/// so far and is what `--samples 513` is for.
const DEFAULT_SAMPLES: u32 = 1025;

/// Peak-to-trough metres the source's range is stretched onto, when nobody says otherwise.
///
/// **Chosen against the slope profile this tool prints, not by eye.** Relief over footprint is the
/// whole of how a map plays — whether a vehicle can climb it, and how much of it the layer rules
/// paint as rock — and the footprint is fixed by the grid, so this is the one number worth turning.
/// Over `Terrain004` at the default 1024 m footprint, the profile moves like this:
///
/// | relief | median slope | 90th | over 35° |
/// |--------|--------------|------|----------|
/// | 45 m   | 6°           | 16°  | 0%       |
/// | 60 m   | 8°           | 21°  | 1%       |
/// | **90 m** | **12°**    | 30°  | **6%**   |
/// | 120 m  | 15°          | 38°  | 13%      |
///
/// 90 is where the map has faces without being made of them. Under it, nothing on the map ever
/// crosses 35° and the rock layer is a rule that never fires — the landscape is real and reads as
/// upholstery. Over it, an eighth of the ground is somewhere a vehicle cannot go, and the routes
/// between the valleys start closing.
///
/// It is a default and not a constant of the game: it is right for *this* footprint. Halving the
/// map to `--samples 513` doubles every gradient — 21% over 35° at the same 90 m — so a smaller
/// map wants a smaller relief to keep the same character.
const DEFAULT_RELIEF: f32 = 90.0;

/// How far past the marker huddle the ground has to stay flat for a place to count as flat, in
/// metres.
///
/// The huddle's own half-diagonal is about 17 m. The margin is what stops a spawn being chosen in
/// a bowl exactly its own size, where every direction anybody walks is uphill.
const SPAWN_MARGIN: f32 = 8.0;

/// How far apart the candidate centres of the flat-spot search stand, in samples.
///
/// A stride rather than every sample, because the cost is the search radius squared per candidate
/// and the answer does not get better for being found to the metre: what is being chosen is a
/// patch of ground tens of metres across, and eight metres of slack in where its middle sits is
/// below the resolution of the question.
const SPAWN_STRIDE: u32 = 8;

/// How much rougher than the best a candidate may be and still be preferred for standing nearer
/// the origin, in metres.
///
/// A tie-break, and it exists because generated terrain has broad plains on which thousands of
/// candidates are flat to the centimetre. Without it the answer is whichever the scan reached
/// first, which is the map's north-west corner and reads as a bug.
const SPAWN_TIE: f32 = 0.05;

fn main() {
    if let Err(trouble) = run() {
        eprintln!("import_heightmap: {trouble}");
        std::process::exit(1);
    }
}

/// What the command line asks for, after defaults.
struct Args {
    source: PathBuf,
    name: String,
    nx: u32,
    nz: u32,
    spacing: f32,
    relief: f32,
    min_y: f32,
    max_y: f32,
    /// Where the marker huddle goes, if the search is not to choose.
    spawn_at: Option<(f32, f32)>,
    maps: PathBuf,
    /// Where to draw the finished map from above, if anywhere.
    preview: Option<PathBuf>,
    dry_run: bool,
}

/// Where maps live, the same anchor `server/src/maps.rs` uses: the source tree, not the working
/// directory, because `cargo run` runs from wherever it likes.
fn default_maps_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../maps")
}

const USAGE: &str = "\
usage: import_heightmap <heightmap.exr> [options]

  --name <name>       what to call the map (default: the file's own stem, sanitised)
  --samples <n>       samples per axis, or <nx>x<nz> (default: 1025)
  --spacing <metres>  metres between samples (default: 1)
  --relief <metres>   peak-to-trough of the finished map (default: 90)
  --min-y <metres>    floor of the quantisation range (default: -64)
  --max-y <metres>    ceiling of the quantisation range (default: 64)
  --spawn-at <x,z>    put the markers here instead of searching for flat ground
  --maps <dir>        where to write (default: the repository's maps/)
  --preview <file>    also draw the finished map from above, as a PNG
  --dry-run           report what it would write, and write nothing";

fn parse_args() -> Result<Args, String> {
    let mut source = None;
    let mut name = None;
    let mut samples = (DEFAULT_SAMPLES, DEFAULT_SAMPLES);
    let mut spacing = DEFAULT_SPACING;
    let mut relief = DEFAULT_RELIEF;
    let mut min_y = DEFAULT_MIN_Y;
    let mut max_y = DEFAULT_MAX_Y;
    let mut spawn_at = None;
    let mut maps = default_maps_dir();
    let mut preview = None;
    let mut dry_run = false;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        // Taken by closure rather than read ahead, so a flag missing its value says which flag.
        let value = |argv: &mut dyn Iterator<Item = String>| {
            argv.next().ok_or_else(|| format!("{arg} wants a value\n\n{USAGE}"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--name" => name = Some(value(&mut argv)?),
            "--samples" => samples = parse_samples(&value(&mut argv)?)?,
            "--spacing" => spacing = parse_number(&value(&mut argv)?, "--spacing")?,
            "--relief" => relief = parse_number(&value(&mut argv)?, "--relief")?,
            "--min-y" => min_y = parse_number(&value(&mut argv)?, "--min-y")?,
            "--max-y" => max_y = parse_number(&value(&mut argv)?, "--max-y")?,
            "--spawn-at" => spawn_at = Some(parse_pair(&value(&mut argv)?)?),
            "--maps" => maps = PathBuf::from(value(&mut argv)?),
            "--preview" => preview = Some(PathBuf::from(value(&mut argv)?)),
            "--dry-run" => dry_run = true,
            other if other.starts_with('-') => return Err(format!("no such option {other}\n\n{USAGE}")),
            other if source.is_none() => source = Some(PathBuf::from(other)),
            other => return Err(format!("only one heightmap at a time, and {other} is a second\n\n{USAGE}")),
        }
    }

    let source = source.ok_or_else(|| format!("no heightmap given\n\n{USAGE}"))?;
    // The file's own stem through the same rule a player's typed name goes through, so the tool
    // cannot put a name in `maps/` that the server's own scan would then refuse to list.
    let name = match name {
        Some(given) => given,
        None => source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| "the heightmap's name is not text; pass --name".to_string())?
            .to_string(),
    };
    let name = sanitise_name(&name).map_err(|fault| format!("{name:?} is not a map name: {fault}"))?;

    Ok(Args {
        source,
        name,
        nx: samples.0,
        nz: samples.1,
        spacing,
        relief,
        min_y,
        max_y,
        spawn_at,
        maps,
        preview,
        dry_run,
    })
}

fn parse_number(text: &str, flag: &str) -> Result<f32, String> {
    text.parse::<f32>().map_err(|_| format!("{flag} wants a number, not {text:?}"))
}

/// `1025` or `1025x513`. Rectangles exist because `Grid` allows them and a heightmap of a valley
/// is not always square.
fn parse_samples(text: &str) -> Result<(u32, u32), String> {
    let count = |part: &str| {
        part.parse::<u32>().map_err(|_| format!("--samples wants whole numbers, not {text:?}"))
    };
    match text.split_once(['x', 'X']) {
        Some((nx, nz)) => Ok((count(nx)?, count(nz)?)),
        None => {
            let n = count(text)?;
            Ok((n, n))
        }
    }
}

fn parse_pair(text: &str) -> Result<(f32, f32), String> {
    let (x, z) = text
        .split_once(',')
        .ok_or_else(|| format!("--spawn-at wants x,z, not {text:?}"))?;
    Ok((parse_number(x.trim(), "--spawn-at")?, parse_number(z.trim(), "--spawn-at")?))
}

fn run() -> Result<(), String> {
    let args = parse_args()?;

    let grid = Grid {
        nx: args.nx,
        nz: args.nz,
        spacing: args.spacing,
        // Centred on the world origin, exactly as `Grid::new` centres what it derives. Not built
        // through `Grid::new` itself because that takes an extent and derives the counts, and here
        // the counts are what an author picked.
        origin_x: -((args.nx.max(1) - 1) as f32) * args.spacing / 2.0,
        origin_z: -((args.nz.max(1) - 1) as f32) * args.spacing / 2.0,
        min_y: args.min_y,
        max_y: args.max_y,
    };
    grid.check().map_err(|fault| format!("that is not a grid this game will load: {fault}"))?;

    // Refused rather than clamped. `quantise` saturates, so an over-tall relief comes out as a map
    // with its summits sheared flat — which looks like a plateau somebody authored, and is the kind
    // of wrong that is discovered a week later.
    if args.relief <= 0.0 || args.relief > grid.span() {
        return Err(format!(
            "a relief of {} m does not fit in a range of {} m ({} to {}); \
             raise --min-y/--max-y or lower --relief",
            args.relief,
            grid.span(),
            grid.min_y,
            grid.max_y,
        ));
    }

    let source = read_heightmap(&args.source)?;
    println!(
        "read {} — {}x{} px, channel {:?}, values {:.4} to {:.4}",
        args.source.display(),
        source.width,
        source.height,
        source.channel,
        source.low,
        source.high,
    );
    if source.width < grid.nx as usize || source.height < grid.nz as usize {
        eprintln!(
            "warning: {}x{} px is smaller than the {}x{} grid asked for; \
             this resamples by area and will not invent detail it does not have",
            source.width, source.height, grid.nx, grid.nz,
        );
    }

    let unit = resample(&source, grid.nx, grid.nz);
    let heights = fit(&unit, &grid, args.relief);

    let markers = place_markers(&grid, &heights, args.spawn_at)?;
    let terrain = Terrain {
        grid,
        // Dry. Water is everywhere at `water_y` or nowhere at all, and terrain.md's open question
        // about swimming off the edge of the field is not one an import gets to answer by default.
        water_y: None,
        heights,
        // The default look, which is also what an empty `layers` in the manifest means. Written
        // that way rather than repeated here, so an imported map is not pinned to today's set.
        layers: Vec::new(),
        markers,
    };

    report(&terrain, args.relief);

    if let Some(path) = &args.preview {
        draw(&terrain, path)?;
        println!("drew {}", path.display());
    }

    let manifest = Manifest {
        version: VERSION,
        grid: terrain.grid,
        water_y: terrain.water_y,
        layers: Vec::new(),
        markers: terrain.markers.clone(),
    };
    // Asked the same questions the server will ask on the way in, here, where the answer is a
    // message rather than a map that silently fails to appear in somebody's menu.
    manifest.check().map_err(|fault| format!("the map this made is one the server would refuse: {fault}"))?;

    let heights_path = args.maps.join(format!("{}.heights", args.name));
    let manifest_path = args.maps.join(format!("{}.json", args.name));
    if args.dry_run {
        println!("\n--dry-run: would write {} and {}", heights_path.display(), manifest_path.display());
        return Ok(());
    }

    std::fs::create_dir_all(&args.maps).map_err(|error| format!("cannot make {}: {error}", args.maps.display()))?;
    let text = serde_json::to_string_pretty(&manifest).map_err(|error| error.to_string())?;
    // Heights first, manifest second — the reverse of the order in `server/src/maps.rs`, and
    // deliberately: `maps::scan` lists a map by its *manifest*, so a run killed between the two
    // writes leaves a heights file nothing reads, rather than a map in the menu with no heights
    // behind it. Overwriting an existing map is the same story, one file at a time.
    std::fs::write(&heights_path, terrain.encode())
        .map_err(|error| format!("cannot write {}: {error}", heights_path.display()))?;
    std::fs::write(&manifest_path, text)
        .map_err(|error| format!("cannot write {}: {error}", manifest_path.display()))?;

    println!(
        "\nwrote {} ({:.1} MB) and {}",
        heights_path.display(),
        terrain.grid.samples() as f64 * 2.0 / 1.0e6,
        manifest_path.display(),
    );

    read_back(&heights_path, &manifest_path, &terrain)?;
    println!("read both back: the server will load this as {:?}", args.name);
    Ok(())
}

/// Opens the pair that was just written, exactly the way `server/src/maps.rs` opens one.
///
/// Not belt and braces. Everything before this is *this program's* opinion that it produced a map;
/// the only thing that settles it is parsing the manifest, putting it through `Manifest::check`
/// and handing the blob to `Terrain::decode` — the three steps a load is made of — and finding the
/// same heights and the same markers come back out. It costs a re-read of a couple of megabytes,
/// and it is the difference between a run that says it wrote a map and a run that knows it did.
///
/// The markers are compared *through the file*, which is where the manifest's `yaw` shorthand lives
/// (see `terrain::marker_file`): a rotation that could not survive the round trip would show up
/// here rather than as a vehicle facing the wrong way on somebody's screen.
fn read_back(heights: &Path, manifest: &Path, expected: &Terrain) -> Result<(), String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|error| format!("cannot read back {}: {error}", manifest.display()))?;
    let read: Manifest = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a manifest: {error}", manifest.display()))?;
    read.check().map_err(|fault| format!("{} would be refused: {fault}", manifest.display()))?;

    let blob = std::fs::read(heights)
        .map_err(|error| format!("cannot read back {}: {error}", heights.display()))?;
    let back = Terrain::decode(read.grid, read.water_y, &blob)
        .map_err(|fault| format!("{} does not decode: {fault}", heights.display()))?;

    if back.heights != expected.heights {
        return Err(format!("{} came back with different heights", heights.display()));
    }
    if read.markers != expected.markers {
        return Err(format!("{} came back with different markers", manifest.display()));
    }
    Ok(())
}

/// A heightmap as this tool holds it: one channel of `f32`, row-major, top row first.
struct Heightmap {
    width: usize,
    height: usize,
    channel: String,
    values: Vec<f32>,
    low: f32,
    high: f32,
}

/// Reads the one channel of an EXR that carries height.
///
/// **Which channel, and why it is not simply the first.** A heightmap is written out by whatever
/// made it, and the conventions do not agree: a luminance-only file calls it `Y`, a greyscale file
/// written through an RGB pipeline calls it `R` and repeats it three times, and a displacement map
/// baked beside other data may call it `Z` or `height`. Preferring in that order and falling back
/// to the first channel present is what makes all four work without a flag — and the channel that
/// was actually used is printed, so a file that fools it says so on the way past.
fn read_heightmap(path: &Path) -> Result<Heightmap, String> {
    use exr::prelude::*;

    let image = read_first_flat_layer_from_file(path)
        .map_err(|error| format!("cannot read {} as an OpenEXR image: {error}", path.display()))?;
    let layer = image.layer_data;
    let (width, height) = (layer.size.width(), layer.size.height());

    let channels = &layer.channel_data.list;
    let named = |wanted: &str| {
        channels.iter().position(|channel| channel.name.to_string().eq_ignore_ascii_case(wanted))
    };
    let index = named("Y")
        .or_else(|| named("R"))
        .or_else(|| named("Z"))
        .or_else(|| named("height"))
        .or(if channels.is_empty() { None } else { Some(0) })
        .ok_or_else(|| format!("{} has no channels at all", path.display()))?;
    let channel = &channels[index];

    // Every sample width an EXR may hold, widened to `f32`. `u32` is divided by its own maximum
    // rather than cast, because an integer channel is a fixed-point unit range and casting it
    // would produce heights in the billions that the fit below would then dutifully rescale.
    let values: Vec<f32> = match &channel.sample_data {
        FlatSamples::F32(samples) => samples.clone(),
        FlatSamples::F16(samples) => samples.iter().map(|half| half.to_f32()).collect(),
        FlatSamples::U32(samples) => {
            samples.iter().map(|&n| n as f32 / u32::MAX as f32).collect()
        }
    };
    if values.len() != width * height {
        return Err(format!(
            "{} says it is {width}x{height} but channel {:?} has {} samples",
            path.display(),
            channel.name.to_string(),
            values.len(),
        ));
    }

    // NaN in a heightmap is a hole, and there is no such thing as a hole in a height field — every
    // sample has to be a number the collider can stand on. Refused rather than patched, because
    // guessing what should be there is the importer inventing terrain.
    let mut low = f32::INFINITY;
    let mut high = f32::NEG_INFINITY;
    for &value in &values {
        if !value.is_finite() {
            return Err(format!("{} has samples that are not numbers", path.display()));
        }
        low = low.min(value);
        high = high.max(value);
    }

    Ok(Heightmap { width, height, channel: channel.name.to_string(), values, low, high })
}

/// The source at the grid's sample positions, still in the source's own units.
///
/// **Area-weighted, and separably so.** Every source pixel that falls under a target sample
/// contributes in proportion to how much of it does, which is the filter that makes an 8× reduction
/// of an 8K heightmap read as the same landscape rather than as one pixel in sixty-four of it. The
/// difference is not subtle at this ratio: point sampling a ridge line eight metres apart lands on
/// the ridge in some columns and beside it in others, and the ridge comes out as a row of bumps.
///
/// Separable because the filter is a product of two intervals: `nx * height` values across, then
/// `nx * nz` down, instead of a box per sample. On 8192² into 1025² that is 8 million weighted adds
/// rather than 68 million.
fn resample(source: &Heightmap, nx: u32, nz: u32) -> Vec<f32> {
    let across = taps(source.width, nx);
    let down = taps(source.height, nz);

    let mut rows = vec![0.0f32; nx as usize * source.height];
    for row in 0..source.height {
        let line = &source.values[row * source.width..(row + 1) * source.width];
        for (ix, tap) in across.iter().enumerate() {
            rows[row * nx as usize + ix] = tap.apply(line, 1);
        }
    }

    let mut out = vec![0.0f32; nx as usize * nz as usize];
    for (iz, tap) in down.iter().enumerate() {
        for ix in 0..nx as usize {
            out[iz * nx as usize + ix] = tap.apply(&rows[ix..], nx as usize);
        }
    }
    out
}

/// One output sample's worth of input: where it starts and what each input weighs.
struct Tap {
    first: usize,
    weights: Vec<f32>,
}

impl Tap {
    /// The weighted mean of `values[first..]`, taken every `stride`.
    fn apply(&self, values: &[f32], stride: usize) -> f32 {
        let mut sum = 0.0;
        let mut total = 0.0;
        for (step, &weight) in self.weights.iter().enumerate() {
            sum += values[(self.first + step) * stride] * weight;
            total += weight;
        }
        // `total` is a sum of overlaps that is positive by construction — the interval always meets
        // at least one pixel — so this is a normalisation and not a guard against division by zero.
        sum / total
    }
}

/// The filter for one axis: `n` output samples across `pixels` input pixels.
///
/// Output sample `i` sits at input coordinate `i * (pixels - 1) / (n - 1)`, so the first and last
/// land exactly on the first and last pixel — the grid's corners are the image's corners, which is
/// what makes two maps imported from the same source at different resolutions line up. Its footprint
/// is half the spacing to each side, and at least half a pixel, so an upsample degenerates to
/// nearest-neighbour rather than to an empty sum.
fn taps(pixels: usize, n: u32) -> Vec<Tap> {
    let last = (pixels - 1) as f64;
    let steps = (n - 1).max(1) as f64;
    let radius = (last / steps / 2.0).max(0.5);
    (0..n)
        .map(|i| {
            let centre = i as f64 * last / steps;
            let from = (centre - radius).max(-0.5);
            let to = (centre + radius).min(last + 0.5);
            let first = (from + 0.5).floor().max(0.0) as usize;
            let after = ((to + 0.5).ceil() as usize).min(pixels);
            let weights = (first..after)
                .map(|pixel| {
                    // How much of this pixel's own unit-wide footprint the interval covers.
                    let overlap = to.min(pixel as f64 + 0.5) - from.max(pixel as f64 - 0.5);
                    overlap.max(0.0) as f32
                })
                .collect();
            Tap { first, weights }
        })
        .collect()
}

/// The resampled field as the map's own `u16` samples.
///
/// The source's range is stretched onto `relief` metres and centred on y = 0 — *centred*, so the
/// same import at a bigger relief grows in both directions rather than sinking, and so a map has
/// the same headroom above its summits as it has below its valleys to be sculpted into.
///
/// A dead flat source is the one case with no range to stretch, and it comes out at the midpoint,
/// which is exactly where `Terrain::new` starts a map nobody has touched.
fn fit(unit: &[f32], grid: &Grid, relief: f32) -> Vec<u16> {
    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    for &value in unit {
        low = low.min(value);
        high = high.max(value);
    }
    let middle = (low + high) / 2.0;
    let scale = if high > low { relief / (high - low) } else { 0.0 };
    unit.iter().map(|&value| grid.quantise((value - middle) * scale)).collect()
}

/// The default marker set, moved on to ground that will hold it.
///
/// The set is moved **as one piece**: the eight spawns, the two vehicles and the three crates keep
/// the layout `level::default_markers` gives them, and only the ground under them is chosen. Moving
/// each marker to its own flat sample instead would scatter the arrangement and lose the one thing
/// about it that was designed — that a player walking forward from a spawn reaches a vehicle.
fn place_markers(grid: &Grid, heights: &[u16], at: Option<(f32, f32)>) -> Result<Vec<Marker>, String> {
    let mut markers = default_markers();
    if markers.is_empty() {
        return Ok(markers);
    }

    // What the huddle occupies, from the markers themselves rather than from a constant, so it
    // stays right when the default set changes.
    let (mut low, mut high) = ((f32::INFINITY, f32::INFINITY), (f32::NEG_INFINITY, f32::NEG_INFINITY));
    for marker in &markers {
        low = (low.0.min(marker.x), low.1.min(marker.z));
        high = (high.0.max(marker.x), high.1.max(marker.z));
    }
    let middle = ((low.0 + high.0) / 2.0, (low.1 + high.1) / 2.0);
    let reach = ((high.0 - low.0).max(high.1 - low.1) / 2.0) + SPAWN_MARGIN;

    let (centre, roughness) = match at {
        Some(given) => (given, spread(grid, heights, given, reach)),
        None => flattest(grid, heights, reach)?,
    };
    println!(
        "markers put at ({:.0}, {:.0}) — the ground within {reach:.0} m of it rises and falls {roughness:.2} m",
        centre.0, centre.1,
    );

    for marker in &mut markers {
        marker.x += centre.0 - middle.0;
        marker.z += centre.1 - middle.1;
        marker.check(*grid).map_err(|fault| {
            format!("a {} marker landed off the map at ({}, {}): {fault}", marker.kind, marker.x, marker.z)
        })?;
    }
    Ok(markers)
}

/// How much the ground within `reach` metres of a point rises and falls, in metres.
///
/// Peak-to-trough rather than a mean gradient, and that is the property being asked for: a spawn
/// wants ground that is *level*, and a slope of a steady two degrees is level enough to stand a
/// vehicle on where a two-metre step in the middle of an otherwise flat field is not.
fn spread(grid: &Grid, heights: &[u16], centre: (f32, f32), reach: f32) -> f32 {
    let span = (reach / grid.spacing).ceil() as i64;
    let index = |world: f32, origin: f32| ((world - origin) / grid.spacing).round() as i64;
    let (cx, cz) = (index(centre.0, grid.origin_x), index(centre.1, grid.origin_z));
    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    for iz in (cz - span)..=(cz + span) {
        for ix in (cx - span)..=(cx + span) {
            let (ix, iz) = (ix.clamp(0, grid.nx as i64 - 1) as u32, iz.clamp(0, grid.nz as i64 - 1) as u32);
            let y = grid.height(heights[grid.index(ix, iz)]);
            low = low.min(y);
            high = high.max(y);
        }
    }
    high - low
}

/// The flattest place on the map that will hold the marker huddle.
///
/// Two passes over the same candidates: the first finds the flattest, the second takes the one
/// nearest the world origin among everything within [`SPAWN_TIE`] of it. On the sort of terrain
/// this imports, the second pass is what actually decides — a landscape has plains, thousands of
/// candidates sit on them at the same roughness to the centimetre, and "flattest" alone would
/// resolve that by scan order and drop everybody in a corner of the map.
fn flattest(grid: &Grid, heights: &[u16], reach: f32) -> Result<((f32, f32), f32), String> {
    // Kept a full reach clear of the rim, so the huddle does not hang over the edge of the field
    // and `spread` does not measure ground it had to clamp to find.
    let inset = ((reach / grid.spacing).ceil() as u32) + 1;
    if grid.nx <= 2 * inset || grid.nz <= 2 * inset {
        return Err(format!(
            "a {:.0} x {:.0} m map is too small to stand the default markers on; use --spawn-at",
            grid.extent_x(),
            grid.extent_z(),
        ));
    }

    let mut candidates = Vec::new();
    let mut best = f32::INFINITY;
    let mut iz = inset;
    while iz < grid.nz - inset {
        let mut ix = inset;
        while ix < grid.nx - inset {
            let world = grid.world_of(ix, iz);
            let roughness = spread(grid, heights, (world.x, world.y), reach);
            best = best.min(roughness);
            candidates.push(((world.x, world.y), roughness));
            ix += SPAWN_STRIDE;
        }
        iz += SPAWN_STRIDE;
    }

    candidates
        .into_iter()
        .filter(|(_, roughness)| *roughness <= best + SPAWN_TIE)
        .min_by(|a, b| {
            let distance = |((x, z), _): &((f32, f32), f32)| x * x + z * z;
            distance(a).total_cmp(&distance(b))
        })
        .ok_or_else(|| "found nowhere at all to put the markers".to_string())
}

/// Where the light comes from in the preview: over the shoulder from the north-west, and high.
///
/// Not straight down, which would light every slope by its steepness alone and lose which *way*
/// each one faces — the thing that makes a picture of terrain read as terrain rather than as a
/// contour map. North-west because it is the convention every relief map has used for a century,
/// and because a landscape lit from the other side reads inside-out to most people: ridges look
/// like gullies.
const SUN: [f32; 3] = [-0.55, 0.68, -0.48];

/// How much of the preview is ambient rather than sun, so that ground facing away is still legible.
const AMBIENT: f32 = 0.28;

/// Draws the finished map from above, in the colours it will actually wear.
///
/// **The layer rules do the colouring, not a height ramp.** `default_layers` and `surface_of` are
/// the game's own answer to "what is this ground", so the preview is grass where the map will be
/// grass and rock where it will be rock — which makes it a check on `--relief` and not merely a
/// picture: it is the only way to see, before loading anything, whether the rock layer is firing
/// across half the map or nowhere at all.
///
/// It is drawn from the *authored* height field, which is what the two files contain. The relief
/// steep ground carries at play time is a function of position added on top of this, and it is a
/// metre of detail on a picture whose pixels are a metre wide.
///
/// One pixel per sample, and the image is laid out the way the source was — sample (0, 0) top left
/// — so it can be put beside the heightmap it came from.
fn draw(terrain: &Terrain, path: &Path) -> Result<(), String> {
    let grid = terrain.grid;
    let layers = default_layers();
    // What the shore rule is measured against. An imported map has no water yet, so this is the
    // waterline that is nowhere and the preview comes out with no beach on it — which is honestly
    // what the map looks like until somebody sets one.
    let sea = waterline(terrain.water_y);
    let mut pixels = vec![0u8; grid.samples() * 3];

    for iz in 0..grid.nz {
        for ix in 0..grid.nx {
            let y = terrain.height_at(ix, iz);
            let west = terrain.height_at(ix.saturating_sub(1), iz);
            let east = terrain.height_at((ix + 1).min(grid.nx - 1), iz);
            let south = terrain.height_at(ix, iz.saturating_sub(1));
            let north = terrain.height_at(ix, (iz + 1).min(grid.nz - 1));
            let up = 2.0 * grid.spacing;
            let (dx, dz) = (west - east, south - north);
            let length = (dx * dx + up * up + dz * dz).sqrt();
            let normal = [dx / length, up / length, dz / length];

            let colour = surface_of(&layers, normal[1], y, terrain.dip_at(ix, iz), y - sea)
                .map_or(NOTHING, |index| layers[index].colour);
            let sun = (normal[0] * SUN[0] + normal[1] * SUN[1] + normal[2] * SUN[2]).max(0.0);
            let lit = AMBIENT + (1.0 - AMBIENT) * sun;

            let at = grid.index(ix, iz) * 3;
            for channel in 0..3 {
                pixels[at + channel] = srgb(colour[channel] * lit);
            }
        }
    }

    // The markers last, over the ground rather than under it. A cross and a ring, because thirteen
    // markers in a thirty-metre huddle are four pixels on a 1025-pixel map: the crosses say what is
    // there and the ring is what the eye finds from across the picture.
    for marker in &terrain.markers {
        stamp(&mut pixels, &grid, marker.x, marker.z, Mark::Cross);
    }
    if !terrain.markers.is_empty() {
        // The bounding box's middle rather than the mean of the positions, because that is what
        // `place_markers` moved on to the flat ground: eight of the thirteen are player spawns in a
        // row, and a mean would sit on them rather than in the middle of the huddle.
        let (mut low, mut high) =
            ((f32::INFINITY, f32::INFINITY), (f32::NEG_INFINITY, f32::NEG_INFINITY));
        for marker in &terrain.markers {
            low = (low.0.min(marker.x), low.1.min(marker.z));
            high = (high.0.max(marker.x), high.1.max(marker.z));
        }
        stamp(&mut pixels, &grid, (low.0 + high.0) / 2.0, (low.1 + high.1) / 2.0, Mark::Ring);
    }

    image::RgbImage::from_raw(grid.nx, grid.nz, pixels)
        .ok_or_else(|| "the preview came out the wrong size".to_string())?
        .save(path)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// What a sample with no layer over it is drawn as — a mid grey, and it should never appear.
///
/// `default_layers` covers every slope and every hollow between them, so `surface_of` returning
/// nothing means a rule has developed a gap. Grey rather than a panic, because this is a preview.
const NOTHING: [f32; 3] = [0.18, 0.18, 0.18];

/// Linear light to one channel of an 8-bit sRGB image, by the standard's own two-part curve.
///
/// A layer's `colour` is linear — see `terrain::default_layers`, where each was measured off its
/// texture in linear light — and writing a linear value into a PNG is what makes grass come out
/// nearly black. This is the same conversion the renderer does on the way to the screen.
fn srgb(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let encoded = if clamped <= 0.003_130_8 {
        12.92 * clamped
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// What to draw at a place on the preview.
enum Mark {
    Cross,
    Ring,
}

/// Puts one mark on the preview, in white, at a world position.
fn stamp(pixels: &mut [u8], grid: &Grid, x: f32, z: f32, mark: Mark) {
    let px = ((x - grid.origin_x) / grid.spacing).round() as i64;
    let pz = ((z - grid.origin_z) / grid.spacing).round() as i64;
    let mut plot = |dx: i64, dz: i64| {
        let (ix, iz) = (px + dx, pz + dz);
        if ix < 0 || iz < 0 || ix >= grid.nx as i64 || iz >= grid.nz as i64 {
            return;
        }
        let at = (iz as usize * grid.nx as usize + ix as usize) * 3;
        pixels[at..at + 3].fill(0xff);
    };
    match mark {
        Mark::Cross => {
            for step in -2..=2 {
                plot(step, 0);
                plot(0, step);
            }
        }
        // A ring at the radius the flat-spot search actually measured over, so what is marked is
        // the ground that was tested and not a decoration around the middle of it.
        Mark::Ring => {
            let radius = 24.0 / grid.spacing;
            for degree in 0..360 {
                let angle = degree as f32 * core::f32::consts::TAU / 360.0;
                plot((radius * angle.cos()).round() as i64, (radius * angle.sin()).round() as i64);
            }
        }
    }
}

/// What the finished map is, in the numbers an author would change `--relief` against.
///
/// The slope profile is the point of this. The layer rules in `shared` paint rock from 35°, a
/// vehicle stops climbing somewhere near 30°, and a player walks up almost anything — so "how much
/// of this map is over 35°" is at once how much of it looks like rock and how much of it a vehicle
/// cannot go. Printing the percentiles rather than a mean because terrain is not normally
/// distributed: a map that is 80% valley floor and 20% cliff has a gentle mean and is a wall.
fn report(terrain: &Terrain, relief: f32) {
    let grid = terrain.grid;
    println!(
        "\n{} x {} samples at {} m — a {:.0} x {:.0} m map, {:.0} m of relief in a {:.0} m range",
        grid.nx,
        grid.nz,
        grid.spacing,
        grid.extent_x(),
        grid.extent_z(),
        relief,
        grid.span(),
    );

    let (mut low, mut high) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut slopes = Vec::with_capacity(grid.samples());
    for iz in 0..grid.nz {
        for ix in 0..grid.nx {
            let y = terrain.height_at(ix, iz);
            low = low.min(y);
            high = high.max(y);
            // The same central differences the drawn mesh and the collider take, one sample either
            // side and clamped at the rim — so this is the slope the layer rules will see rather
            // than a second opinion about it.
            let west = terrain.height_at(ix.saturating_sub(1), iz);
            let east = terrain.height_at((ix + 1).min(grid.nx - 1), iz);
            let south = terrain.height_at(ix, iz.saturating_sub(1));
            let north = terrain.height_at(ix, (iz + 1).min(grid.nz - 1));
            // `normal.y` of the cross product of the two tangents, over its length.
            let (dx, dz) = (west - east, south - north);
            let up = 2.0 * grid.spacing;
            slopes.push(slope_degrees(up / (dx * dx + up * up + dz * dz).sqrt()));
        }
    }
    println!("heights {low:.1} m to {high:.1} m");

    slopes.sort_by(f32::total_cmp);
    let at = |fraction: f32| slopes[((slopes.len() - 1) as f32 * fraction) as usize];
    let over = |degrees: f32| {
        let under = slopes.partition_point(|slope| *slope < degrees);
        (slopes.len() - under) as f32 * 100.0 / slopes.len() as f32
    };
    println!(
        "slope: median {:.0}°, 90th {:.0}°, 99th {:.0}°, steepest {:.0}°",
        at(0.5),
        at(0.9),
        at(0.99),
        slopes[slopes.len() - 1],
    );
    println!(
        "       {:.0}% of it is over 35° — rock to the layer rules, and past what a vehicle climbs",
        over(35.0),
    );
}
