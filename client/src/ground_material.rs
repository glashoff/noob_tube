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
use bevy::pbr::{ExtendedMaterial, MaterialExtension};
use bevy::prelude::*;
use bevy::render::render_resource::{AsBindGroup, ShaderType};
use bevy::shader::ShaderRef;
use noob_tube_shared::terrain::{Layer, MAX_LAYERS};

/// The shader, by the path the asset server knows it under.
const SHADER: &str = "shaders/ground.wgsl";

/// How much the world-space noise may darken or lighten the ground, either way.
///
/// Not a texture and not pretending to be one. What it buys is the thing a flat colour cannot: a
/// hillside that still reads as a surface at two hundred metres, and something for the eye to hold
/// on to at walking pace. A tenth is enough to do that and little enough that a slope's *shading*
/// still reads as its shape rather than as dirt.
const DETAIL: f32 = 0.12;

/// The two scales it is mixed at, in metres.
///
/// Far apart on purpose — an octave pair close together reads as one blurry scale. The coarse one
/// is about the size of a hill's shoulder and the fine one about the size of a vehicle, which is
/// what puts a change inside every view whatever the range.
const COARSE_METRES: f32 = 42.0;
const FINE_METRES: f32 = 3.5;

/// What the ground is drawn with: Bevy's PBR, with the derivation bolted on to its fragment stage.
pub type GroundMaterial = ExtendedMaterial<StandardMaterial, GroundLayers>;

pub struct GroundMaterialPlugin;

impl Plugin for GroundMaterialPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(MaterialPlugin::<GroundMaterial>::default());
    }
}

/// The rules, laid out the way a uniform wants them.
///
/// Packed into `vec4`s because a uniform array of scalars is padded to sixteen bytes an element:
/// the unpacked form is four times the size and says nothing more. The layout is mirrored in
/// `assets/shaders/ground.wgsl` and nowhere else.
#[derive(Clone, Copy, Default, ShaderType, Debug, Reflect)]
pub struct GroundRules {
    /// `rgb` linear colour, `w` perceptual roughness.
    colour: [Vec4; MAX_LAYERS],
    /// Slope band in degrees from flat: from, to, blend, unused.
    slope: [Vec4; MAX_LAYERS],
    /// Height band in metres of world y: from, to, blend, unused.
    height: [Vec4; MAX_LAYERS],
    /// How many of the rows are real. A map with more layers than the cap loses the extras here
    /// rather than in the shader, where the loop bound is this number.
    count: u32,
    detail: f32,
    coarse_metres: f32,
    fine_metres: f32,
}

impl GroundRules {
    /// The uniform a map's layers describe.
    pub fn of(layers: &[Layer]) -> Self {
        let mut rules = Self { detail: DETAIL, coarse_metres: COARSE_METRES, fine_metres: FINE_METRES, ..default() };
        for (slot, layer) in layers.iter().take(MAX_LAYERS).enumerate() {
            let [r, g, b] = layer.colour;
            rules.colour[slot] = Vec4::new(r, g, b, layer.roughness);
            rules.slope[slot] =
                Vec4::new(layer.slope.from, layer.slope.to, layer.slope.blend, 0.0);
            rules.height[slot] =
                Vec4::new(layer.height.from, layer.height.to, layer.height.blend, 0.0);
            rules.count += 1;
        }
        rules
    }
}

/// The extension itself: one uniform and one fragment shader.
#[derive(Asset, AsBindGroup, Reflect, Clone, Debug)]
pub struct GroundLayers {
    // 100 and up is the range Bevy leaves free for an extension; the base material owns 0..99.
    #[uniform(100)]
    pub rules: GroundRules,
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

/// A handle the material is built under, so that a rebuild replaces it rather than leaking one.
pub const GROUND_MATERIAL: Handle<GroundMaterial> =
    uuid_handle!("6f2a1f4e-8c3d-4a19-9b77-1f0c5a2e7d31");

/// The material for a map's layers, written into the one handle the ground uses.
///
/// One material for the whole map rather than one per tile: every tile evaluates the same rules,
/// and a material per tile would be sixty-four bind groups saying the same thing.
pub fn dress(materials: &mut Assets<GroundMaterial>, layers: &[Layer]) -> Handle<GroundMaterial> {
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
            extension: GroundLayers { rules: GroundRules::of(layers) },
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
