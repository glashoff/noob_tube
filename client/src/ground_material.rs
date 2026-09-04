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

use bevy::asset::{RenderAssetUsages, uuid_handle};
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, Extent3d, ShaderType, TextureDimension};
use bevy::shader::ShaderRef;
use noob_tube_shared::terrain::{Layer, MAX_LAYERS};

/// The shader, by the path the asset server knows it under.
const SHADER: &str = "shaders/ground.wgsl";

/// What the ground is drawn with: Bevy's PBR, with the derivation bolted on to its fragment stage.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundLayers>;

pub struct GroundMaterialPlugin;

impl Plugin for GroundMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<GroundMaterial>::default())
            .init_resource::<GroundTextures>()
            // Chained, and the order is the whole of it: the mip levels are built into each
            // layer's own image, and the stack copies those images as they then are. Stacking
            // first would put four one-level textures into an array that says it has eleven.
            .add_systems(Update, (build_the_mipmaps, stack_the_layers).chain());
    }
}

/// The images the ground is using, and the two array textures made out of them.
///
/// **The shader reads array textures, not one binding per layer**, and that is not tidiness. A
/// fragment stage may use sixteen sampled textures on WebGPU — the figure Chrome reports on every
/// adapter, whatever the hardware underneath — and Bevy's PBR pass has spent most of them before
/// this material is reached. Four colour maps and four packed maps of our own took the total to
/// eighteen: the pipeline was refused, the opaque pass failed, and the browser client quit with a
/// validation error. Two arrays cost two.
///
/// So the per-layer images loaded here are raw material rather than what is bound. They are
/// gathered, given their mip levels, and copied into one array texture each — see
/// [`stack_the_layers`], which is also where the reason they cannot be bound one at a time is
/// written down.
#[derive(Resource, Default)]
pub struct GroundTextures {
    /// One entry per layer slot, in slot order.
    pub layers: Vec<LayerImages>,
    /// The colour array and the detail array, once there are any. Held so that the next map drops
    /// them rather than leaving two textures on the GPU nothing points at.
    stacked: Option<[Handle<Image>; 2]>,
    /// Whether the attempt has been made. A stack that could not be built — layers of different
    /// sizes, say — must not be retried on every frame for the life of the map.
    settled: bool,
}

/// What one layer brought with it: a colour map, and the packed detail beside it.
///
/// `None` means the layer never asked for one — an untextured layer, drawn in its own average
/// colour. A handle that is present but whose file is missing is a different thing again, and the
/// difference is only known once the asset server has tried: see [`stack_the_layers`].
#[derive(Default)]
pub struct LayerImages {
    /// The colour map, or `None` for a layer with no texture at all.
    pub colour: Option<Handle<Image>>,
    /// The packed detail map beside it, if `tools/bake_ground_maps` has made one.
    pub packed: Option<Handle<Image>>,
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
    /// Slope band in degrees from flat: from, to, blend, and `w` = 1 once the colour array holds
    /// this layer's map.
    ///
    /// The flag is needed because a *missing* texture is not distinguishable in the shader: an
    /// unfilled binding is a white placeholder, and white is also what a texture that happens to be
    /// white looks like. Without it a layer whose map has not arrived — or never will, because it
    /// has none — would come out white rather than in its own colour. [`stack_the_layers`] writes
    /// it, for the same reason the detail flag beside it cannot be decided here.
    slope: [Vec4; MAX_LAYERS],
    /// Height band in metres of world y: from, to, blend, and `w` = metres one texture tile spans.
    height: [Vec4; MAX_LAYERS],
    /// Hollow band in metres below the surroundings: from, to, blend.
    ///
    /// `w` = 1 once the detail array holds this layer's packed map. Whether a `_Packed.png` exists
    /// at all is a question about the disk, and a layer that has none must come out exactly as it
    /// did before they existed rather than as noise. [`stack_the_layers`] writes it.
    dip: [Vec4; MAX_LAYERS],
    /// How many of the rows are real. A map with more layers than the cap loses the extras here
    /// rather than in the shader, where the loop bound is this number.
    ///
    /// Last, and with no padding after it: both `ShaderType` and WGSL round a struct's size up to
    /// its own alignment, which the `Vec4` arrays already put at sixteen bytes.
    count: u32,
}

impl GroundRules {
    /// The uniform a map's layers describe.
    pub fn of(layers: &[Layer]) -> Self {
        let mut rules = Self::default();
        for (slot, layer) in layers.iter().take(MAX_LAYERS).enumerate() {
            let [r, g, b] = layer.colour;
            rules.colour[slot] = Vec4::new(r, g, b, layer.roughness);
            // The `w` stays zero until the colour array really holds this layer — see the field.
            rules.slope[slot] = Vec4::new(layer.slope.from, layer.slope.to, layer.slope.blend, 0.0);
            rules.height[slot] =
                Vec4::new(layer.height.from, layer.height.to, layer.height.blend, layer.tile_scale);
            // The `w` stays zero until the packed texture is really loaded — see the field.
            rules.dip[slot] = Vec4::new(layer.dip.from, layer.dip.to, layer.dip.blend, 0.0);
            rules.count += 1;
        }
        rules
    }
}

/// The extension itself: one uniform, two array textures, one sampler, and one fragment shader.
///
/// **Two textures rather than eight, because sixteen is the ceiling.** A WebGPU fragment stage may
/// sample sixteen textures, and Chrome reports that number on every adapter regardless of the
/// hardware behind it — Bevy already asks for the adapter's own maximum and is given sixteen on a
/// desktop GPU. Bevy's PBR pass spends most of them on shadow maps, the environment map and the
/// tonemapping LUT; a binding per layer took the total to eighteen and the pipeline was refused
/// outright. See [`GroundTextures`].
///
/// One array holds every layer's colour map and the other every layer's packed detail, indexed by
/// the layer slot — which is also why the shader's four unrolled branches could collapse into a
/// loop: a texture *array* can be indexed by a value the shader computes, where a list of separate
/// bindings cannot.
///
/// **One sampler for both arrays, and that too is deliberate.** A layer's two images want the
/// identical descriptor — the same repeat, the same anisotropy, read at the same uv in the same
/// call — so the detail array declares none of its own. Samplers have a ceiling of sixteen as well,
/// and this material now spends one.
///
/// `None` binds Bevy's own white array placeholder, which is what holds the bindings open while the
/// images are still loading. Nothing reads it: the uniform's per-layer flags stay zero until
/// [`stack_the_layers`] has really put something there.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct GroundLayers {
    // 100 and up is the range Bevy leaves free for an extension; the base material owns 0..99.
    #[uniform(100)]
    pub rules: GroundRules,
    #[texture(101, dimension = "2d_array")]
    #[sampler(102)]
    pub colours: Option<Handle<Image>>,
    #[texture(103, dimension = "2d_array")]
    pub details: Option<Handle<Image>>,
}

/// Loads one of a layer's maps, with the two settings that matter.
///
/// **Repeat, not clamp.** Bevy's default sampler clamps at the edge, and a clamped texture tiled
/// across five hundred metres of ground is one four-metre square in the middle with its edge pixels
/// smeared to the horizon. It has to be set at load time, because the sampler belongs to the image
/// rather than to the material that uses it.
///
/// **Loaded into main memory and not on to the GPU.** See the `asset_usage` line below.
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
            // Never uploaded on its own. These are the raw material `stack_the_layers` copies
            // into the two array textures that *are* bound, and a per-layer image on the GPU
            // beside its own copy inside an array would be the memory paid twice.
            settings.asset_usage = RenderAssetUsages::MAIN_WORLD;
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
        let ours = ground.layers.iter().any(|layer| {
            [&layer.colour, &layer.packed]
                .into_iter()
                .flatten()
                .any(|handle| handle.id() == *id)
        });
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

/// Update: copies each layer's maps into the two array textures the shader actually reads.
///
/// **Why an array at all** is [`GroundLayers`]: sixteen sampled textures per fragment stage is what
/// WebGPU offers, Bevy's PBR pass has spent most of them, and a binding per layer put the total at
/// eighteen — a pipeline that is refused rather than a pipeline that is slow.
///
/// **Why it happens here, once, rather than at load time** is that an array texture is a single
/// object. It cannot be filled in layer by layer as images arrive: it is created with its size, its
/// format and its level count, and everything that goes into it has to agree about all three. So
/// this waits until every image a map asked for has settled — arrived, or failed and never coming —
/// and then copies them all at once. Until then the ground is drawn in the layers' average colours,
/// which is what it was already drawn in for the second before a texture arrived.
///
/// **A layer with nothing to put in it still takes a slot.** An untextured layer, or one whose file
/// is missing, is filled with white. That is never read — the uniform's flag for it stays zero —
/// and the alternative, a shorter array with a slot map beside it, is a second index to keep in
/// step for the sake of four megabytes in a case that is already the fallback.
fn stack_the_layers(
    assets: Res<AssetServer>,
    mut ground: ResMut<GroundTextures>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<GroundMaterial>>,
) {
    use bevy::asset::LoadState;

    if ground.settled || ground.layers.is_empty() {
        return;
    }
    // Every image has to have settled first, and "failed" counts: a `_Packed.png` that was never
    // baked would otherwise hold the whole map's textures back for ever.
    let mut arrived = Vec::new();
    for layer in &ground.layers {
        for handle in [&layer.colour, &layer.packed] {
            let Some(handle) = handle else {
                arrived.push(None);
                continue;
            };
            match assets.load_state(handle) {
                LoadState::Loaded => arrived.push(Some(handle.clone())),
                LoadState::Failed(_) => arrived.push(None),
                // Still on its way. Nothing is built this frame.
                _ => return,
            }
        }
    }

    let colour: Vec<_> = arrived.iter().step_by(2).cloned().collect();
    let packed: Vec<_> = arrived.iter().skip(1).step_by(2).cloned().collect();
    let (Some(colours), Some(details)) = (stack(&images, &colour), stack(&images, &packed)) else {
        // Nothing usable to stack — every layer untextured, or the maps disagree about their size.
        // `stack` has said which; this only makes sure it is not said again sixty times a second.
        ground.settled = true;
        return;
    };
    let colours = images.add(colours);
    let details = images.add(details);

    let Some(mut material) = materials.get_mut(&GROUND_MATERIAL) else {
        return;
    };
    for (slot, (colour, packed)) in colour.iter().zip(&packed).enumerate() {
        material.extension.rules.slope[slot].w = f32::from(colour.is_some());
        material.extension.rules.dip[slot].w = f32::from(packed.is_some());
    }
    material.extension.colours = Some(colours.clone());
    material.extension.details = Some(details.clone());
    ground.stacked = Some([colours, details]);
    ground.settled = true;
    debug!("stacked {} ground layers into two array textures", colour.len());
}

/// One array texture out of one image per layer, or `None` if there is nothing to make one from.
///
/// The shape is taken from the first image that is really there, and every other has to match it —
/// same size, same format, same number of mip levels. A layer that does not (a pack at another
/// resolution, a `_Packed.png` from an older bake) is refused rather than stretched: an array
/// texture has one size, and quietly padding one layer into it would misread every texel of it.
///
/// Slots with no image are filled with white. Nothing samples them; see the caller.
fn stack(images: &Assets<Image>, sources: &[Option<Handle<Image>>]) -> Option<Image> {
    let first = sources
        .iter()
        .flatten()
        .find_map(|handle| images.get(handle))
        .filter(|image| image.data.is_some())?;
    let size = first.texture_descriptor.size;
    let format = first.texture_descriptor.format;
    let levels = first.texture_descriptor.mip_level_count;
    let bytes = first.data.as_ref().map_or(0, Vec::len);

    let mut data = Vec::with_capacity(bytes * sources.len());
    for (slot, handle) in sources.iter().enumerate() {
        let image = handle.as_ref().and_then(|handle| images.get(handle));
        match image {
            Some(image) if image.data.is_some() => {
                let descriptor = &image.texture_descriptor;
                if descriptor.size != size
                    || descriptor.format != format
                    || descriptor.mip_level_count != levels
                {
                    warn!(
                        "ground layer {slot} is {:?} at {} mip levels where the first layer is \
                         {:?} at {levels}; its texture is left off",
                        descriptor.size, descriptor.mip_level_count, size,
                    );
                    data.extend(std::iter::repeat_n(u8::MAX, bytes));
                    continue;
                }
                data.extend_from_slice(image.data.as_ref().expect("checked just above"));
            }
            // No image, or one whose data has been dropped. White, and never read.
            _ => data.extend(std::iter::repeat_n(u8::MAX, bytes)),
        }
    }

    // `new_uninit` rather than `new`, which debug-asserts that the data is exactly one mip level
    // of one layer — this is every level of every layer, laid out the way `TextureDataOrder`'s
    // default wants it: each layer's whole chain, one layer after the next.
    let mut stacked = Image::new_uninit(
        Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: sources.len() as u32,
        },
        TextureDimension::D2,
        format,
        RenderAssetUsages::RENDER_WORLD,
    );
    stacked.texture_descriptor.mip_level_count = levels;
    // The sampler belongs to the image rather than to the material, so the array has to carry the
    // one the per-layer images were loaded with — repeat, and the anisotropy the ground needs
    // because it is seen edge-on for most of the screen. See `load_map`.
    stacked.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        anisotropy_clamp: 8,
        ..ImageSamplerDescriptor::linear()
    });
    stacked.data = Some(data);
    Some(stacked)
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
) -> Handle<GroundMaterial> {
    // A new map starts the whole business again: different layers, different files, and two array
    // textures built out of the old ones that nothing will read. Dropping the handles here is what
    // frees them.
    *ours = GroundTextures::default();
    ours.layers = layers
        .iter()
        .take(MAX_LAYERS)
        .map(|layer| {
            let (colour, packed) = load_layer(assets, &layer.texture);
            LayerImages { colour, packed }
        })
        .collect();

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
                // Filled in by `stack_the_layers` once every image has settled. Until then the
                // bindings hold Bevy's white array placeholder and the uniform's flags keep the
                // shader off it.
                colours: None,
                details: None,
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
    use noob_tube_shared::terrain::default_layers;

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

    /// Every layer's maps are the same shape as every other layer's.
    ///
    /// This is what an array texture demands and what nothing else did. Before the eight bindings
    /// became two arrays, a pack at another resolution was a layer that looked wrong; now it is a
    /// layer that cannot go into the array at all, and `stack` leaves it white. The check belongs
    /// here rather than only in that warning, because the moment to find out is when somebody
    /// swaps a pack — not when somebody notices the ground has gone pale.
    #[test]
    fn every_layer_is_the_same_size_as_every_other_layer() {
        let mut shape: Option<((u32, u32), String)> = None;
        for layer in default_layers() {
            if layer.texture.is_empty() {
                continue;
            }
            for suffix in ["Color", "Packed"] {
                let path = format!("../assets/textures/{}_{suffix}.png", layer.texture);
                let found = image::open(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
                match &shape {
                    None => shape = Some((found.dimensions(), path)),
                    Some((size, first)) => assert_eq!(
                        found.dimensions(),
                        *size,
                        "{path} is {:?} but {first} is {size:?}; one array texture cannot hold \
                         both, and the odd one out is dropped",
                        found.dimensions(),
                    ),
                }
            }
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
            // Both flags start at zero, whatever the layer says it has. What they mean is not
            // "this layer wants a texture" but "the array really holds one for it", which nothing
            // here can know: see `stack_the_layers`.
            assert_eq!(rules.slope[slot].w, 0.0, "a layer claimed a texture before one had arrived");
            assert_eq!(rules.dip[slot].w, 0.0, "a layer claimed detail before it had arrived");
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
