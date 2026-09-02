// The ground's look, derived rather than painted.
//
// Which surface shows at a pixel is a function of that pixel's slope and its height — terrain.md's
// "Surface appearance is derived, and there is no splat map". The *bands* are not written here:
// they are uploaded from `Layer` in `shared/src/terrain.rs`, so this file and the Rust that goes
// with it cannot disagree about where a layer starts. Only the arithmetic between them lives twice,
// which is what terrain.md §10 says has to.
//
// There is no texture sampling yet and so no triplanar and no tile break: with nothing to project
// there is no projection to stretch. What breaks the flatness instead is value noise in *world*
// space, which has the property triplanar exists to buy — it cannot stretch on a slope, because it
// was never in UV space to begin with.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
}

// One layer per row, four rows, matching `MAX_LAYERS`. Packed into vec4s because a uniform array of
// scalars is padded to sixteen bytes an element and would be four times the size for no gain.
struct GroundRules {
    // rgb: linear colour. w: perceptual roughness.
    colour: array<vec4<f32>, 4>,
    // Slope band in degrees from flat: from, to, blend, unused.
    slope: array<vec4<f32>, 4>,
    // Height band in metres of world y: from, to, blend, unused.
    height: array<vec4<f32>, 4>,
    // How many of the four rows are real.
    count: u32,
    // How strong the noise is, and how many metres across its two scales are.
    detail: f32,
    coarse_metres: f32,
    fine_metres: f32,
}

// The group is a shader def rather than a number: Bevy moved the material bind group to 3 in this
// version, and a hard-coded 2 compiles fine and then fails at pipeline build time with "not
// available in the pipeline layout". Bindings from 100 up are the range an extension owns; the base
// material has 0..99.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> rules: GroundRules;

/// A band with a soft edge, exactly as `Band::weight` computes it in Rust.
///
/// `low` and `high` rather than `from` and `to`, because `from` is a reserved word in WGSL — it
/// compiles in Rust, reads the same in both files, and fails at pipeline build time with an error
/// nothing but running the game will show you.
fn band(x: f32, low: f32, high: f32, blend: f32) -> f32 {
    let width = max(blend, 1.0e-4);
    let rising = clamp((x - low) / width + 0.5, 0.0, 1.0);
    let falling = clamp((high - x) / width + 0.5, 0.0, 1.0);
    return min(rising, falling);
}

/// A hash with no pattern the eye can find at the scales this is used at.
fn hash(cell: vec2<f32>) -> f32 {
    let h = fract(sin(dot(cell, vec2<f32>(127.1, 311.7))) * 43758.5453);
    return h;
}

/// Value noise: the four corners of a cell, blended with a smoothstep.
fn noise(at: vec2<f32>) -> f32 {
    let cell = floor(at);
    let f = fract(at);
    let w = f * f * (3.0 - 2.0 * f);
    let a = hash(cell);
    let b = hash(cell + vec2<f32>(1.0, 0.0));
    let c = hash(cell + vec2<f32>(0.0, 1.0));
    let d = hash(cell + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, w.x), mix(c, d, w.x), w.y);
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let world = in.world_position.xyz;
    let normal = normalize(in.world_normal);
    // Degrees from flat, which is what the bands are written in — see `Layer::slope`.
    let slope = degrees(acos(clamp(normal.y, -1.0, 1.0)));

    var colour = vec3<f32>(0.0);
    var roughness = 0.0;
    var total = 0.0;
    for (var i = 0u; i < rules.count; i = i + 1u) {
        let s = rules.slope[i];
        let h = rules.height[i];
        let weight = band(slope, s.x, s.y, s.z) * band(world.y, h.x, h.y, h.z);
        colour += rules.colour[i].rgb * weight;
        roughness += rules.colour[i].w * weight;
        total += weight;
    }
    // A point outside every band is possible and must not come out black: the last layer standing
    // is a better answer than a hole, and a map whose rules leave a gap should look wrong in a way
    // that says "gap" rather than "unlit".
    if total > 1.0e-4 {
        colour /= total;
        roughness /= total;
    } else {
        colour = vec3<f32>(0.5, 0.0, 0.5);
        roughness = 1.0;
    }

    // Two scales of world-space noise, multiplied over the whole thing: the coarse one is what
    // makes a hillside stop looking like one flat colour at a distance, and the fine one is what
    // gives the eye something at walking range.
    let coarse = noise(world.xz / max(rules.coarse_metres, 0.01));
    let fine = noise(world.xz / max(rules.fine_metres, 0.01));
    let shade = 1.0 + rules.detail * (coarse * 0.65 + fine * 0.35 - 0.5) * 2.0;
    colour *= clamp(shade, 0.0, 2.0);

    pbr_input.material.base_color = vec4<f32>(colour, 1.0);
    pbr_input.material.perceptual_roughness = clamp(roughness, 0.05, 1.0);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
