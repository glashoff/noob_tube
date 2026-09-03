//! Fold a ground pack's normal, roughness and displacement maps into the one texture the shader
//! samples beside the colour.
//!
//! `assets/shaders/ground.wgsl` is triplanar and tiles stochastically, so every map it reads costs
//! three samples per world plane, up to three planes, for each of up to four layers. Reading
//! normal, roughness and displacement as three textures would be four times the fetches of colour
//! alone. Packed into one RGBA texture it is twice, and twice is what the shader was extended to.
//!
//! ```text
//! tools/fetch-ground-textures --maps all Ground048        # the four maps, out of the kept pack
//! cargo run -p bake_ground_maps -- Ground048_1K-PNG       # -> Ground048_1K-PNG_Packed.png
//! ```
//!
//! The name is a pack at a resolution and a format, exactly as ambientCG writes it and exactly as
//! a `Layer::texture` field carries it — the same string the colour map is found by.
//!
//! **Channels.** `R` and `G` are the tangent-space normal's x and y; `B` is roughness; `A` is
//! displacement. The shader reads the first three today. The fourth is baked ahead of the step
//! that will use it — blending layers by which one's high points win at a boundary, so that gravel
//! comes through grass rather than dissolving into it — because a channel that is already in the
//! file costs nothing, and re-baking every pack later would.
//!
//! Three things about that layout are decisions rather than convenience:
//!
//! - **`NormalGL`, never `NormalDX`.** wgpu's tangent space has +y pointing up the way OpenGL's
//!   does, and a DirectX map is the same image with y inverted. Nothing about the mistake looks
//!   broken: the surface simply lights as though every bump were a dent, from a sun on the wrong
//!   side, and it takes a known shape lit from a known angle to see it at all.
//! - **The normal's z is dropped** and rebuilt in the shader as `sqrt(1 - x² - y²)`. A unit vector
//!   in the upper hemisphere carries no information in its third component, and the byte is worth
//!   more as displacement than as arithmetic already done.
//! - **Written as data, not as colour.** ambientCG's roughness and displacement maps are greyscale
//!   PNGs with no transfer function applied, and the normal is a vector. The client loads this with
//!   `is_srgb: false`, so what is written here is what the shader reads.
//!
//! What is lost: ambientCG's normals are sixteen bits a channel and this writes eight. On a smooth
//! surface that would band visibly; on gravel and dirt, where the normal changes by more between
//! neighbouring texels than a quantisation step, it does not — and the maps this exists for are all
//! gravel and dirt. Should a smooth one ever need it, the packing is the thing to reconsider, not
//! the bit depth: RG16 has no room for the other two.

use image::{ImageReader, RgbaImage};
use std::path::{Path, PathBuf};

/// Where the maps are read from and the packed one is written, relative to the workspace root.
const TEXTURES: &str = "assets/textures";

fn main() -> std::process::ExitCode {
    let names: Vec<String> = std::env::args().skip(1).collect();
    if names.is_empty() || names.iter().any(|n| n.starts_with('-')) {
        eprintln!(
            "usage: {} <PACK_RESOLUTION-FORMAT>...\n\
             \n\
             e.g. {0} Ground048_1K-PNG Grass001_1K-PNG\n\
             \n\
             The four maps have to be unpacked first, which is one command:\n\
             \x20 tools/fetch-ground-textures --maps all Ground048 Grass001",
            env!("CARGO_BIN_NAME")
        );
        return std::process::ExitCode::FAILURE;
    }
    let mut failed = false;
    for name in &names {
        match bake(name) {
            Ok(report) => println!("{report}"),
            Err(why) => {
                eprintln!("\x1b[31m{name}: {why}\x1b[0m");
                failed = true;
            }
        }
    }
    if failed { std::process::ExitCode::FAILURE } else { std::process::ExitCode::SUCCESS }
}

/// One pack's four maps in, one packed texture out, and a line about what went into it.
fn bake(name: &str) -> Result<String, String> {
    let normal = read(name, "NormalGL")?;
    let roughness = read(name, "Roughness")?;
    let displacement = read(name, "Displacement")?;

    let (wide, high) = normal.dimensions();
    for (what, image) in [("Roughness", &roughness), ("Displacement", &displacement)] {
        if image.dimensions() != (wide, high) {
            let (w, h) = image.dimensions();
            return Err(format!(
                "NormalGL is {wide}×{high} but {what} is {w}×{h}. These have to be the one pack at \
                 the one resolution: a mismatch means two downloads got mixed."
            ));
        }
    }

    let mut packed = RgbaImage::new(wide, high);
    // Sums for the report. A packed texture is hard to look at — three of its channels mean
    // nothing as colour — so the one thing printed is what it averages to, which is enough to
    // catch a map that arrived empty or fully white.
    let (mut mean_rough, mut mean_high) = (0.0f64, 0.0f64);
    for y in 0..high {
        for x in 0..wide {
            let n = normal.get_pixel(x, y).0;
            let r = roughness.get_pixel(x, y).0[0];
            let d = displacement.get_pixel(x, y).0[0];
            packed.put_pixel(x, y, image::Rgba([n[0], n[1], r, d]));
            mean_rough += f64::from(r);
            mean_high += f64::from(d);
        }
    }
    let texels = f64::from(wide) * f64::from(high);

    let out = path(name, "Packed");
    packed.save(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    Ok(format!(
        "{}  {wide}×{high}, {:.1} MB — roughness averages {:.2}, height {:.2}",
        out.display(),
        size as f64 / 1e6,
        mean_rough / texels / 255.0,
        mean_high / texels / 255.0,
    ))
}

/// One of a pack's maps, as 8-bit RGBA whatever it was on disk.
///
/// ambientCG ships the normal as 16-bit RGB and the other two as 16-bit greyscale; `to_rgba8`
/// is where the depth is given up, in one place and knowingly.
fn read(name: &str, map: &str) -> Result<image::RgbaImage, String> {
    let from = path(name, map);
    let file = ImageReader::open(&from).map_err(|e| {
        format!(
            "{}: {e}\n\nUnpack the maps first — it costs no download while the pack is kept:\n  \
             tools/fetch-ground-textures --maps all {}",
            from.display(),
            name.split('_').next().unwrap_or(name)
        )
    })?;
    Ok(file.decode().map_err(|e| format!("{}: {e}", from.display()))?.to_rgba8())
}

fn path(name: &str, map: &str) -> PathBuf {
    Path::new(TEXTURES).join(format!("{name}_{map}.png"))
}
