//! The material the ground wears, and the one place its rules reach the GPU.
//!
//! The rules themselves are map content and live in
//! [`Layer`](noob_tube_shared::terrain::Layer). This is the plumbing that gets them into a uniform
//! and puts the shader that reads it on the tiles.
//!
//! **The numbers cross once.** terrain.md §10 says the surface classification has to exist twice —
//! once on the CPU, for footsteps and decals, and once in WGSL, because the server has no renderer
//! and cannot evaluate a fragment shader. What it does not have to do is exist twice as *data*: the
//! bands are uploaded from the same `Layer` values the Rust side reads, so the two copies can only
//! disagree about the arithmetic between them, never about where a layer starts.

use bevy::asset::uuid_handle;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use noob_tube_shared::terrain::{Layer, MAX_LAYERS};

/// The shader, by the path the asset server knows it under.
const SHADER: &str = "shaders/ground.wgsl";

/// How strongly the tile break modulates the ground, from 0 (off) to 1.
///
/// A four-metre texture repeated over five hundred reads as a grid from anywhere far enough away
/// to see more than a few tiles of it, and no amount of filtering hides a pattern that is really
/// there. The break is a **second sample of the same texture at a much larger scale**, multiplied
/// in — so what varies across a hillside is the texture's own structure, at the size of a
/// landscape feature, rather than a smooth wash laid over the top of it. Synthetic noise stood
/// here before and could only change the brightness; this changes what the ground is made of,
/// which is what the eye was missing.
///
/// `webgame`'s `uTileBreak`, and its value: strong enough to break the grid, weak enough that a
/// slope's shading still reads as its shape.
const TILE_BREAK: f32 = 0.35;

/// What the ground is drawn with: Bevy's PBR, with the derivation bolted on to its fragment stage.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundLayers>;

pub struct GroundMaterialPlugin;

impl Plugin for GroundMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<GroundMaterial>::default())
            .init_resource::<GroundTextures>()
            .add_systems(Update, build_the_mipmaps);
    }
}

/// The colour maps the ground is using, so the mipmap pass knows which images are its business.
#[derive(Resource, Default)]
pub struct GroundTextures(pub Vec<Handle<Image>>);

/// The rules, laid out the way a uniform wants them.
///
/// Packed into `vec4`s because a uniform array of scalars is padded to sixteen bytes an element:
/// the unpacked form is four times the size and says nothing more. The layout is mirrored in
/// `assets/shaders/ground.wgsl` and nowhere else.
#[derive(Clone, Copy, Default, ShaderType, Debug, Reflect)]
pub struct GroundRules {
    /// `rgb` linear colour, `w` perceptual roughness.
    colour: [Vec4; MAX_LAYERS],
    /// Slope band in degrees from flat: from, to, blend, and `w` = 1 where a texture is bound.
    ///
    /// The flag is needed because a *missing* texture is not distinguishable in the shader: Bevy
    /// binds a white 1×1 image in place of one, and white is also what a texture that happens to
    /// be white looks like. Without the flag a layer with no texture would come out white rather
    /// than in its own colour.
    slope: [Vec4; MAX_LAYERS],
    /// Height band in metres of world y: from, to, blend, and `w` = metres one texture tile spans.
    height: [Vec4; MAX_LAYERS],
    /// Hollow band in metres below the surroundings: from, to, blend. `w` is unused.
    dip: [Vec4; MAX_LAYERS],
    /// How many of the rows are real. A map with more layers than the cap loses the extras here
    /// rather than in the shader, where the loop bound is this number.
    count: u32,
    tile_break: f32,
    /// Padding to the sixteen bytes a uniform's tail is rounded up to anyway. Named rather than
    /// implicit, because `ShaderType` and the WGSL struct have to agree on the size and a silent
    /// pad is a silent chance for them not to.
    _pad: Vec2,
}

impl GroundRules {
    /// The uniform a map's layers describe.
    pub fn of(layers: &[Layer]) -> Self {
        let mut rules = Self { tile_break: TILE_BREAK, ..default() };
        for (slot, layer) in layers.iter().take(MAX_LAYERS).enumerate() {
            let [r, g, b] = layer.colour;
            rules.colour[slot] = Vec4::new(r, g, b, layer.roughness);
            let textured = f32::from(!layer.texture.is_empty());
            rules.slope[slot] =
                Vec4::new(layer.slope.from, layer.slope.to, layer.slope.blend, textured);
            rules.height[slot] =
                Vec4::new(layer.height.from, layer.height.to, layer.height.blend, layer.tile_scale);
            rules.dip[slot] = Vec4::new(layer.dip.from, layer.dip.to, layer.dip.blend, 0.0);
            rules.count += 1;
        }
        rules
    }
}

/// The extension itself: one uniform, four textures, and one fragment shader.
///
/// Four separate pairs rather than an array, because WGSL cannot index a list of textures with a
/// loop variable — so the shader unrolls, and the binding layout follows it. `None` binds Bevy's
/// own white placeholder, which is why the uniform carries a flag saying whether a layer really
/// has one.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct GroundLayers {
    // 100 and up is the range Bevy leaves free for an extension; the base material owns 0..99.
    #[uniform(100)]
    pub rules: GroundRules,
    #[texture(101)]
    #[sampler(102)]
    pub texture_0: Option<Handle<Image>>,
    #[texture(103)]
    #[sampler(104)]
    pub texture_1: Option<Handle<Image>>,
    #[texture(105)]
    #[sampler(106)]
    pub texture_2: Option<Handle<Image>>,
    #[texture(107)]
    #[sampler(108)]
    pub texture_3: Option<Handle<Image>>,
}

/// Loads one layer's colour map, with the one sampler setting that matters.
///
/// **Repeat, not clamp.** Bevy's default sampler clamps at the edge, and a clamped texture tiled
/// across five hundred metres of ground is one four-metre square in the middle with its edge pixels
/// smeared to the horizon. It has to be set at load time, because the sampler belongs to the image
/// rather than to the material that uses it.
fn load_colour(assets: &AssetServer, texture: &str) -> Option<Handle<Image>> {
    if texture.is_empty() {
        return None;
    }
    // ambientCG's own file naming, kept exactly: the name says which pack, at what resolution, in
    // what format, and which map of the pack it is. See `assets/CREDITS.md`.
    let path = format!("textures/{texture}_Color.png");
    Some(
        assets
            .load_builder()
            .with_settings(|settings: &mut ImageLoaderSettings| {
                settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                    address_mode_u: ImageAddressMode::Repeat,
                    address_mode_v: ImageAddressMode::Repeat,
                    // Ground is the one thing in this game seen almost edge-on for most of the
                    // screen, which is exactly the case a plain mip chain over-blurs: the level is
                    // picked for the *worse* of the two axes, so a hillside running to the horizon
                    // is filtered as though it were as compressed across as it is along.
                    anisotropy_clamp: 8,
                    ..ImageSamplerDescriptor::linear()
                });
            })
            .load(path),
    )
}

impl MaterialExtension for GroundLayers {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    /// The depth prepass keeps the base material's shader on purpose.
    ///
    /// The prepass writes depth and normals and never asks what colour anything is, so running the
    /// derivation there would be the whole rule set evaluated for nothing, once per pixel, on a
    /// pass whose entire job is to be cheap.
    fn prepass_fragment_shader() -> ShaderRef {
        ShaderRef::Default
    }
}

/// Update: gives the ground's textures the mip levels the PNG loader does not.
///
/// **Bevy does not generate mipmaps for a PNG**, and a 4 m texture tiled across five hundred metres
/// without them does not look soft in the distance — it *boils*. Every frame the camera moves, each
/// far-away pixel lands on a different texel of a 1024² image, and the ground crawls. The
/// alternative is shipping KTX2 with the levels baked in, which means a conversion tool in the
/// build and a second copy of every texture; this is thirty lines and happens once per image.
///
/// Averaged in **linear** light, not in sRGB. A box filter over sRGB bytes is the classic way to
/// get mipmaps that are visibly darker than the image they came from — the mean of two encoded
/// values is not the encoding of their mean — and the error compounds with every level, so the
/// horizon ends up a different colour from the ground underfoot.
fn build_the_mipmaps(
    ground: Res<GroundTextures>,
    mut events: MessageReader<AssetEvent<Image>>,
    mut images: ResMut<Assets<Image>>,
) {
    for event in events.read() {
        let AssetEvent::LoadedWithDependencies { id } = event else {
            continue;
        };
        if !ground.0.iter().any(|handle| handle.id() == *id) {
            continue;
        }
        let Some(mut image) = images.get_mut(*id) else {
            continue;
        };
        match add_mip_levels(&mut image) {
            Some(levels) => debug!("built {levels} mip levels for a ground texture"),
            None => warn!("a ground texture kept its one mip level and will shimmer"),
        }
    }
}

/// Halves an RGBA8 image down to 1×1, appending each level to its data. Returns the level count.
fn add_mip_levels(image: &mut Image) -> Option<usize> {
    use bevy::render::render_resource::TextureFormat;
    if image.texture_descriptor.mip_level_count > 1 {
        return Some(image.texture_descriptor.mip_level_count as usize);
    }
    // Only the one format the PNG loader produces for a colour map. Anything else is a texture
    // this was not written for, and guessing at its layout would corrupt it silently.
    if image.texture_descriptor.format != TextureFormat::Rgba8UnormSrgb {
        return None;
    }
    let (mut wide, mut high) = (
        image.texture_descriptor.size.width as usize,
        image.texture_descriptor.size.height as usize,
    );
    let mut data = image.data.take()?;
    let mut level = data.clone();
    let mut levels = 1;
    while wide > 1 || high > 1 {
        let (next_wide, next_high) = ((wide / 2).max(1), (high / 2).max(1));
        let mut smaller = vec![0u8; next_wide * next_high * 4];
        for y in 0..next_high {
            for x in 0..next_wide {
                // The four texels of the level above, or the two, or the one — a non-square image
                // runs out along one axis first.
                let (x0, x1) = (x * 2, (x * 2 + 1).min(wide - 1));
                let (y0, y1) = (y * 2, (y * 2 + 1).min(high - 1));
                for channel in 0..4 {
                    let at = |x: usize, y: usize| level[(y * wide + x) * 4 + channel];
                    let mean = if channel == 3 {
                        // Alpha is linear already and must not be gamma-corrected.
                        let sum = at(x0, y0) as u32
                            + at(x1, y0) as u32
                            + at(x0, y1) as u32
                            + at(x1, y1) as u32;
                        (sum / 4) as u8
                    } else {
                        let sum = srgb_to_linear(at(x0, y0))
                            + srgb_to_linear(at(x1, y0))
                            + srgb_to_linear(at(x0, y1))
                            + srgb_to_linear(at(x1, y1));
                        linear_to_srgb(sum / 4.0)
                    };
                    smaller[(y * next_wide + x) * 4 + channel] = mean;
                }
            }
        }
        data.extend_from_slice(&smaller);
        level = smaller;
        (wide, high) = (next_wide, next_high);
        levels += 1;
    }
    image.texture_descriptor.mip_level_count = levels as u32;
    image.data = Some(data);
    Some(levels)
}

/// The sRGB transfer function and its inverse, on a byte.
fn srgb_to_linear(value: u8) -> f32 {
    let v = value as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn linear_to_srgb(value: f32) -> u8 {
    let v = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// A handle the material is built under, so that a rebuild replaces it rather than leaking one.
pub const GROUND_MATERIAL: Handle<GroundMaterial> =
    uuid_handle!("6f2a1f4e-8c3d-4a19-9b77-1f0c5a2e7d31");

/// The material for a map's layers, written into the one handle the ground uses.
///
/// One material for the whole map rather than one per tile: every tile evaluates the same rules,
/// and a material per tile would be sixty-four bind groups saying the same thing.
pub fn dress(
    materials: &mut Assets<GroundMaterial>,
    assets: &AssetServer,
    ours: &mut GroundTextures,
    layers: &[Layer],
) -> Handle<GroundMaterial> {
    let mut texture = [const { None }; MAX_LAYERS];
    for (slot, layer) in layers.iter().take(MAX_LAYERS).enumerate() {
        texture[slot] = load_colour(assets, &layer.texture);
    }
    ours.0 = texture.iter().flatten().cloned().collect();
    let [texture_0, texture_1, texture_2, texture_3] = texture;
    // The insert can only fail on a handle whose asset has been dropped mid-frame, which this one
    // cannot be: it is a constant, and the tiles that hold it are spawned in the same call.
    let written = materials.insert(
        &GROUND_MATERIAL,
        ExtendedMaterial {
            base: StandardMaterial {
                // The base colour is multiplied into whatever the extension writes, so it has to be
                // white: anything else would tint a derivation that has already decided.
                base_color: Color::WHITE,
                perceptual_roughness: 1.0,
                ..default()
            },
            extension: GroundLayers {
                rules: GroundRules::of(layers),
                texture_0,
                texture_1,
                texture_2,
                texture_3,
            },
        },
    );
    if let Err(error) = written {
        error!("the ground's material could not be written: {error}");
    }
    GROUND_MATERIAL
}

#[cfg(test)]
mod tests {
    use super::*;
    use noob_tube_shared::terrain::default_layers;

    /// Every band reaches the uniform in the units the shader reads them in.
    ///
    /// The one thing that can silently go wrong in the crossing: a degree written as a cosine, a
    /// blend dropped, a layer past the cap counted anyway. None of those would fail to compile and
    /// all of them would show as ground of the wrong colour, which is a slow thing to diagnose by
    /// looking.
    #[test]
    fn the_rules_reach_the_uniform_as_they_were_written() {
        let layers = default_layers();
        let rules = GroundRules::of(&layers);
        assert_eq!(rules.count as usize, layers.len());
        for (slot, layer) in layers.iter().enumerate() {
            assert_eq!(rules.colour[slot].xyz(), Vec3::from_array(layer.colour));
            assert_eq!(rules.colour[slot].w, layer.roughness);
            assert_eq!(rules.slope[slot].x, layer.slope.from);
            assert_eq!(rules.slope[slot].y, layer.slope.to);
            assert_eq!(rules.slope[slot].z, layer.slope.blend);
            assert_eq!(rules.height[slot].x, layer.height.from);
            assert_eq!(rules.height[slot].y, layer.height.to);
            assert_eq!(rules.height[slot].z, layer.height.blend);
            assert_eq!(rules.height[slot].w, layer.tile_scale, "the tile scale did not cross");
            assert_eq!(rules.dip[slot].x, layer.dip.from);
            assert_eq!(rules.dip[slot].y, layer.dip.to);
            assert_eq!(rules.dip[slot].z, layer.dip.blend);
            assert_eq!(rules.slope[slot].w, 1.0, "a layer with a texture was marked as having none");
        }
    }

    /// A map with more layers than the shader has room for loses the extras here, where it can be
    /// reasoned about, rather than in a loop reading past the end of an array.
    #[test]
    fn more_layers_than_the_cap_are_dropped_rather_than_overrunning() {
        let mut layers = default_layers();
        while layers.len() <= MAX_LAYERS {
            layers.push(layers[0].clone());
        }
        let rules = GroundRules::of(&layers);
        assert_eq!(rules.count as usize, MAX_LAYERS);
    }
}
