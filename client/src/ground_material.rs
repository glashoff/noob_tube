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

use bevy::asset::{AssetLoadFailedEvent, uuid_handle};
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use noob_tube_shared::terrain::{Ground, Layer, MAX_LAYERS, waterline};

/// The shader, by the path the asset server knows it under.
const SHADER: &str = "shaders/ground.wgsl";

/// What the ground is drawn with: Bevy's PBR, with the derivation bolted on to its fragment stage.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundLayers>;

pub struct GroundMaterialPlugin;

impl Plugin for GroundMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<GroundMaterial>::default())
            .init_resource::<GroundTextures>()
            .add_systems(
                Update,
                (
                    build_the_mipmaps,
                    detail_has_arrived,
                    detail_is_absent,
                    the_waterline_moved.run_if(resource_exists_and_changed::<Ground>),
                ),
            );
    }
}

/// The images the ground is using, so the passes that finish them know which are their business.
///
/// The packed ones carry the layer slot they belong to, because their *arrival* is what the shader
/// has to be told about: an image that has not loaded is not absent, it is Bevy's white 1x1
/// placeholder, and white is a valid packed texel meaning a normal tipped flat on its side. See
/// [`detail_has_arrived`].
#[derive(Resource, Default)]
pub struct GroundTextures {
    /// Colour maps, which need mip levels and nothing else.
    pub colour: Vec<Handle<Image>>,
    /// Packed detail maps, each with the layer it dresses.
    pub packed: Vec<(usize, Handle<Image>)>,
}

/// The rules, laid out the way a uniform wants them.
///
/// Packed into `vec4`s because a uniform array of scalars is padded to sixteen bytes an element:
/// the unpacked form is four times the size and says nothing more. The layout is mirrored in
/// `assets/shaders/ground.wgsl` and nowhere else.
#[derive(Clone, Copy, Default, ShaderType, Debug, Reflect)]
pub struct GroundRules {
    /// `rgb` the layer's average linear colour, `w` perceptual roughness.
    ///
    /// The average is load-bearing twice over: it is what the ground is painted with before a
    /// texture has loaded, and it is the mean the stochastic blend restores the contrast around.
    /// A wrong one shows up as ground that changes brightness when its texture arrives, and as
    /// washed-out patches in the middle of every lattice triangle.
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
    /// Hollow band in metres below the surroundings: from, to, blend.
    ///
    /// `w` = 1 once the layer's packed detail texture has finished loading. Unlike the colour
    /// flag beside it, this one cannot be decided here: whether a `_Packed.png` exists is a
    /// question about the disk, and a layer that has no detail maps must come out exactly as it
    /// did before they existed rather than as noise. [`detail_has_arrived`] writes it.
    dip: [Vec4; MAX_LAYERS],
    /// Shore band in metres above the waterline: from, to, blend. `w` is spare.
    shore: [Vec4; MAX_LAYERS],
    /// How many of the rows are real. A map with more layers than the cap loses the extras here
    /// rather than in the shader, where the loop bound is this number.
    count: u32,
    /// The world y the shore band is measured from — the map's own water level resolved through
    /// [`waterline`], so that a dry map is a number here rather than a branch in the shader.
    ///
    /// **The one field that is not the layers'**, and the one that changes without the map
    /// changing: an author dragging the waterline expects the beach to come with it, and
    /// [`the_waterline_moved`] writes this rather than rebuilding a material and sixty-four meshes
    /// for one float.
    ///
    /// Last, together with `count`, and with no padding written after the pair: both `ShaderType`
    /// and WGSL round a struct's size up to its own alignment, which the `Vec4` arrays already put
    /// at sixteen bytes — and two four-byte scalars fit inside that tail with room to spare.
    waterline: f32,
}

impl GroundRules {
    /// The uniform a map's layers describe, measured against the map's own waterline.
    pub fn of(layers: &[Layer], water_y: Option<f32>) -> Self {
        let mut rules = Self { waterline: waterline(water_y), ..Self::default() };
        for (slot, layer) in layers.iter().take(MAX_LAYERS).enumerate() {
            let [r, g, b] = layer.colour;
            rules.colour[slot] = Vec4::new(r, g, b, layer.roughness);
            let textured = f32::from(!layer.texture.is_empty());
            rules.slope[slot] =
                Vec4::new(layer.slope.from, layer.slope.to, layer.slope.blend, textured);
            rules.height[slot] =
                Vec4::new(layer.height.from, layer.height.to, layer.height.blend, layer.tile_scale);
            // The `w` stays zero until the packed texture is really loaded — see the field.
            rules.dip[slot] = Vec4::new(layer.dip.from, layer.dip.to, layer.dip.blend, 0.0);
            rules.shore[slot] = Vec4::new(layer.shore.from, layer.shore.to, layer.shore.blend, 0.0);
            rules.count += 1;
        }
        rules
    }
}

/// The extension itself: one uniform, eight textures, four samplers, and one fragment shader.
///
/// Separate fields rather than arrays, because WGSL cannot index a list of textures with a loop
/// variable — so the shader unrolls, and the binding layout follows it. `None` binds Bevy's own
/// white placeholder, which is why the uniform carries a flag per texture saying whether a layer
/// really has one.
///
/// **The packed textures declare no sampler of their own, and that is deliberate twice over.** A
/// layer's two images want the identical descriptor — the same repeat, the same anisotropy, and
/// they are sampled at the same uv in the same call — so the shader reads both through
/// `sampler_i`. And samplers are a limit that is reached: wgpu's floor is sixteen per shader
/// stage, Bevy's PBR fragment stage already spends most of them on shadows, environment maps and
/// the tonemapping LUT, and four more here for nothing would be a pipeline that fails to build on
/// exactly the hardware this is meant to run on.
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
    #[texture(109)]
    pub packed_0: Option<Handle<Image>>,
    #[texture(110)]
    pub packed_1: Option<Handle<Image>>,
    #[texture(111)]
    pub packed_2: Option<Handle<Image>>,
    #[texture(112)]
    pub packed_3: Option<Handle<Image>>,
}

/// Loads one of a layer's maps, with the two settings that matter.
///
/// **Repeat, not clamp.** Bevy's default sampler clamps at the edge, and a clamped texture tiled
/// across five hundred metres of ground is one four-metre square in the middle with its edge pixels
/// smeared to the horizon. It has to be set at load time, because the sampler belongs to the image
/// rather than to the material that uses it.
///
/// **`srgb` says whether the file is a picture or a table.** A colour map is a picture and carries
/// the sRGB transfer function; a packed map is three unrelated quantities stored as bytes — two
/// components of a unit vector, a roughness, a height — and decoding those as though they were
/// colour bends every one of them along a curve that has nothing to do with what they mean. The
/// PNG loader assumes a picture unless it is told otherwise, and this is where it is told.
fn load_map(assets: &AssetServer, path: String, srgb: bool) -> Handle<Image> {
    assets
        .load_builder()
        .with_settings(move |settings: &mut ImageLoaderSettings| {
            settings.is_srgb = srgb;
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
        .load(path)
}

/// A layer's colour map, and the packed one beside it if `tools/bake_ground_maps` has made it.
///
/// Both are asked for unconditionally. A packed map that is not on disk fails to load and stays a
/// white placeholder, and the shader is kept off it by the flag [`detail_has_arrived`] writes —
/// which is also what makes shipping one optional: the three colour maps in this repository are
/// the game, the detail beside them is an improvement on it.
fn load_layer(assets: &AssetServer, texture: &str) -> (Option<Handle<Image>>, Option<Handle<Image>>) {
    if texture.is_empty() {
        return (None, None);
    }
    // ambientCG's own file naming, kept exactly: the name says which pack, at what resolution, in
    // what format, and which map of the pack it is. `_Packed` is this repository's own suffix and
    // the one thing in the name ambientCG did not write — see `tools/bake_ground_maps`.
    (
        Some(load_map(assets, format!("textures/{texture}_Color.png"), true)),
        Some(load_map(assets, format!("textures/{texture}_Packed.png"), false)),
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
        let ours = ground.colour.iter().any(|handle| handle.id() == *id)
            || ground.packed.iter().any(|(_, handle)| handle.id() == *id);
        if !ours {
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

/// Update: gives up on a layer's packed detail map, so that the ground can be drawn without one.
///
/// This is not a nicety, it is what keeps a missing `_Packed.png` from taking the whole ground with
/// it. `AsBindGroup` will not build a bind group while any of its images is still unresolved, and a
/// handle whose file does not exist stays unresolved for ever — so the material is never prepared,
/// the tiles draw nothing at all, and the only sign of it is one `Path not found` in the log. The
/// ground vanishing is not a plausible punishment for not having run a bake tool.
///
/// Dropping the handle is what lets the material through: `None` binds Bevy's white placeholder,
/// which is a perfectly good image to hold a binding open with as long as nothing reads it — and
/// nothing does, because the flag [`detail_has_arrived`] would have set stays zero.
fn detail_is_absent(
    ground: Res<GroundTextures>,
    mut failures: MessageReader<AssetLoadFailedEvent<Image>>,
    mut materials: ResMut<Assets<GroundMaterial>>,
) {
    for failure in failures.read() {
        for (slot, _) in ground.packed.iter().filter(|(_, it)| it.id() == failure.id) {
            let Some(mut material) = materials.get_mut(&GROUND_MATERIAL) else {
                continue;
            };
            let detail = &mut material.extension;
            match slot {
                0 => detail.packed_0 = None,
                1 => detail.packed_1 = None,
                2 => detail.packed_2 = None,
                _ => detail.packed_3 = None,
            }
            debug!("layer {slot} has no detail maps: {}", failure.path);
        }
    }
}

/// Update: lets the shader read a layer's packed detail map, once there is one to read.
///
/// The flag cannot be set where the rest of the uniform is written. A texture that has not loaded
/// is not missing — Bevy binds a white 1×1 image in its place — and white is a perfectly valid
/// packed texel: a tangent normal of (1, 1), which is no unit vector at all, over roughness 1.
/// Reading that would not look like an absent detail map, it would look like the ground had been
/// replaced by something wrong, for the second or two before the real image arrived and for good
/// on any layer whose `_Packed.png` was never baked.
///
/// So the question the shader is answered is not "does this layer have detail maps" but "has one
/// arrived", which is a fact this event carries and no filesystem check could give: `dress` runs
/// before the asset server has opened anything, and a load that fails raises `Failed` instead of
/// this and correctly leaves the flag alone.
fn detail_has_arrived(
    ground: Res<GroundTextures>,
    mut events: MessageReader<AssetEvent<Image>>,
    mut materials: ResMut<Assets<GroundMaterial>>,
) {
    for event in events.read() {
        let AssetEvent::LoadedWithDependencies { id } = event else {
            continue;
        };
        for (slot, _) in ground.packed.iter().filter(|(_, it)| it.id() == *id) {
            let Some(mut material) = materials.get_mut(&GROUND_MATERIAL) else {
                continue;
            };
            material.extension.rules.dip[*slot].w = 1.0;
            debug!("layer {slot} has its detail maps");
        }
    }
}

/// Update: moves the beach when somebody moves the sea.
///
/// The shore band is measured from the waterline rather than from a world y, so the one number it
/// is measured against has to reach the shader every time it changes — and it changes without the
/// map doing so. `dress_the_ground` cannot carry it: that runs on a new *map*, deliberately, since
/// re-dressing the ground is sixty-four meshes and a fifth of a second, and an author dragging the
/// water level would pay it on every frame of the drag.
///
/// So this writes the one float instead. A material fetched mutably re-uploads its uniform, which
/// is the whole cost — no mesh is touched, and the water's own surface is rebuilt by
/// [`water`](crate::water) on the same change.
///
/// **Read through `get` before `get_mut`.** Asking an `Assets` for a mutable handle marks the asset
/// changed whether or not anything is written to it, so a system that reached straight for one
/// would re-upload the uniform on every stroke of a held brush — `Ground` is marked changed by
/// those too. The comparison has to happen on the immutable side to mean anything.
fn the_waterline_moved(ground: Res<Ground>, mut materials: ResMut<Assets<GroundMaterial>>) {
    let now = waterline(ground.0.water_y);
    if materials.get(&GROUND_MATERIAL).is_none_or(|it| it.extension.rules.waterline == now) {
        return;
    }
    if let Some(mut material) = materials.get_mut(&GROUND_MATERIAL) {
        material.extension.rules.waterline = now;
        debug!("the waterline moved to {now} m");
    }
}

/// Halves an RGBA8 image down to 1×1, appending each level to its data. Returns the level count.
///
/// **A packed map is averaged straight, a colour map through the transfer function.** The two
/// formats the PNG loader produces here are the same bytes meaning different things: sRGB for a
/// picture, plain `Unorm` for the table of normals, roughness and heights that
/// `tools/bake_ground_maps` writes. Sending the second through the sRGB curve would bend a normal
/// on its way down the mip chain, which is a lighting error that grows with distance — exactly
/// where nothing is looking closely enough to catch it.
///
/// Averaging a normal's x and y and rebuilding z from them shortens the vector and flattens the
/// surface as the levels go down. That is the right direction: a square metre of gravel really is
/// smoother, as a surface, than the gravel in it. What it does not do is turn that lost bumpiness
/// into roughness the way a Toksvig map would, so a distant slope is a little glossier than it
/// should be — smaller than the shimmer this whole function exists to stop.
fn add_mip_levels(image: &mut Image) -> Option<usize> {
    use bevy::render::render_resource::TextureFormat;
    if image.texture_descriptor.mip_level_count > 1 {
        return Some(image.texture_descriptor.mip_level_count as usize);
    }
    // Only the two formats the PNG loader produces for this material. Anything else is a texture
    // this was not written for, and guessing at its layout would corrupt it silently.
    let encoded = match image.texture_descriptor.format {
        TextureFormat::Rgba8UnormSrgb => true,
        TextureFormat::Rgba8Unorm => false,
        _ => return None,
    };
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
                    let mean = if channel == 3 || !encoded {
                        // Alpha is linear already and must not be gamma-corrected, and neither is
                        // any channel of a packed map.
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
    water_y: Option<f32>,
) -> Handle<GroundMaterial> {
    let mut texture = [const { None }; MAX_LAYERS];
    let mut detail = [const { None }; MAX_LAYERS];
    for (slot, layer) in layers.iter().take(MAX_LAYERS).enumerate() {
        (texture[slot], detail[slot]) = load_layer(assets, &layer.texture);
    }
    ours.colour = texture.iter().flatten().cloned().collect();
    ours.packed = detail
        .iter()
        .enumerate()
        .filter_map(|(slot, handle)| handle.clone().map(|handle| (slot, handle)))
        .collect();
    let [texture_0, texture_1, texture_2, texture_3] = texture;
    let [packed_0, packed_1, packed_2, packed_3] = detail;
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
                rules: GroundRules::of(layers, water_y),
                texture_0,
                texture_1,
                texture_2,
                texture_3,
                packed_0,
                packed_1,
                packed_2,
                packed_3,
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
    use image::GenericImageView;
    use noob_tube_shared::terrain::{NO_WATERLINE, default_layers};

    /// The colour written for each layer is the colour its texture actually averages to.
    ///
    /// [`Layer::colour`] is a measurement, not a preference: the shader restores the contrast of
    /// its stochastic blend *around* this value, and paints untextured ground *with* it. A wrong
    /// one is a texture that changes brightness the moment it loads, and washed-out patches in the
    /// middle of every lattice triangle — neither of which points at a number in a table.
    ///
    /// So the number is checked against the file it came from rather than trusted. It reads the
    /// shipped PNG, converts to linear light exactly as the GPU does for an sRGB texture, and
    /// averages. Swap a pack for another and this fails, which is the moment to want it to.
    #[test]
    fn each_layer_is_the_colour_its_own_texture_averages_to() {
        for layer in default_layers() {
            let path = format!("../assets/textures/{}_Color.png", layer.texture);
            let image = image::open(&path).unwrap_or_else(|e| panic!("{path}: {e}")).to_rgb8();
            let mut total = [0.0f64; 3];
            for pixel in image.pixels() {
                for (sum, channel) in total.iter_mut().zip(pixel.0) {
                    *sum += f64::from(srgb_to_linear(channel));
                }
            }
            let count = f64::from(image.width()) * f64::from(image.height());
            for (channel, sum) in total.iter().enumerate() {
                let measured = sum / count;
                let written = f64::from(layer.colour[channel]);
                assert!(
                    (measured - written).abs() < 0.002,
                    "{} channel {channel} averages {measured:.4}, but the layer says {written:.4}",
                    layer.texture,
                );
            }
        }
    }

    /// Every layer that ships a colour map ships the detail beside it, at the same size.
    ///
    /// The two files are read at one uv by one sampler, and the shader takes the second's word for
    /// which way the surface faces. So a `_Packed.png` that is missing, or that came from a
    /// re-bake at another resolution, is not a smaller version of the same picture — it is a
    /// normal map that no longer lines up with the colour it is lighting, and nothing about the
    /// result says which of the two files is the wrong one.
    ///
    /// Missing is the case that would otherwise stay quiet: the ground still draws, because
    /// [`detail_is_absent`] makes sure it does, and it draws exactly as it did before any of this
    /// existed. That is the right behaviour for a checkout and the wrong one for this repository,
    /// where the file is committed and its absence means someone deleted it.
    #[test]
    fn each_layer_that_has_a_colour_map_has_the_detail_that_goes_with_it() {
        for layer in default_layers() {
            if layer.texture.is_empty() {
                continue;
            }
            let colour = format!("../assets/textures/{}_Color.png", layer.texture);
            let packed = format!("../assets/textures/{}_Packed.png", layer.texture);
            let colour = image::open(&colour).unwrap_or_else(|e| panic!("{colour}: {e}"));
            let detail = image::open(&packed).unwrap_or_else(|e| {
                panic!("{packed}: {e}\n\nBake it: cargo run -p bake_ground_maps -- {}", layer.texture)
            });
            assert_eq!(
                colour.dimensions(),
                detail.dimensions(),
                "{} is {:?} but its detail map is {:?}",
                layer.texture,
                colour.dimensions(),
                detail.dimensions(),
            );
            assert_eq!(
                detail.color(),
                image::ColorType::Rgba8,
                "{}'s detail map is {:?}: the shader reads four channels, and a PNG that dropped \
                 one of them would silently shift roughness into where the height should be",
                layer.texture,
                detail.color(),
            );
        }
    }

    /// Every band reaches the uniform in the units the shader reads them in.
    ///
    /// The one thing that can silently go wrong in the crossing: a degree written as a cosine, a
    /// blend dropped, a layer past the cap counted anyway. None of those would fail to compile and
    /// all of them would show as ground of the wrong colour, which is a slow thing to diagnose by
    /// looking.
    #[test]
    fn the_rules_reach_the_uniform_as_they_were_written() {
        let layers = default_layers();
        let rules = GroundRules::of(&layers, Some(-12.0));
        assert_eq!(rules.count as usize, layers.len());
        assert_eq!(rules.waterline, -12.0, "the waterline did not cross");
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
            assert_eq!(rules.shore[slot].x, layer.shore.from);
            assert_eq!(rules.shore[slot].y, layer.shore.to);
            assert_eq!(rules.shore[slot].z, layer.shore.blend);
            assert_eq!(rules.slope[slot].w, 1.0, "a layer with a texture was marked as having none");
        }

        // And the dry map, which is the case the shader has no branch for: it is told a waterline
        // rather than that there is none, and the number has to be the one the CPU side would use
        // or the two halves of terrain.md §10 disagree about where the beach is.
        assert_eq!(GroundRules::of(&layers, None).waterline, NO_WATERLINE);
    }

    /// A map with more layers than the shader has room for loses the extras here, where it can be
    /// reasoned about, rather than in a loop reading past the end of an array.
    #[test]
    fn more_layers_than_the_cap_are_dropped_rather_than_overrunning() {
        let mut layers = default_layers();
        while layers.len() <= MAX_LAYERS {
            layers.push(layers[0].clone());
        }
        let rules = GroundRules::of(&layers, None);
        assert_eq!(rules.count as usize, MAX_LAYERS);
    }
}
