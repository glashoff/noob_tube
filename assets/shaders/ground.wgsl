// The ground's look, derived rather than painted.
//
// Which surface shows at a pixel is a function of that pixel's slope, its height, and how far it
// sits below the ground around it — terrain.md's "Surface appearance is derived, and there is no
// splat map". The third of those cannot be read off the point itself, so it rides in on the mesh:
// `ground_mesh` writes it into the second uv set, where the standard vertex shader carries it
// through for free.
//
// The *bands* are not written here: they are uploaded from `Layer` in `shared/src/terrain.rs`, so
// this file and the Rust that goes with it cannot disagree about where a layer starts. Only the
// arithmetic between them lives twice, which is what terrain.md §10 says has to.
//
// **Triplanar, and not as an option.** A height field has no UVs and could not use them if it had:
// a texture projected flat on to a 60° ravine wall arrives stretched by a factor of two. So every
// layer is sampled on all three world planes and blended by the surface normal, which costs three
// samples where a stretched one costs one. What makes that affordable is that the projection
// weights collapse: on flat ground the Y plane carries essentially all of it, and the other two
// branches are skipped. `textureSampleGrad` throughout, because a `textureSample` inside a branch
// is not allowed to work out its own mip level — the derivatives are taken up front, in uniform
// control flow, where they are still meaningful.

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
    // Slope band in degrees from flat: from, to, blend, and w = 1 when a texture is bound.
    slope: array<vec4<f32>, 4>,
    // Height band in metres of world y: from, to, blend, w = metres one texture tile spans.
    height: array<vec4<f32>, 4>,
    // Hollow band in metres below the surroundings: from, to, blend. w unused.
    dip: array<vec4<f32>, 4>,
    // How many of the four rows are real.
    count: u32,
    // How strongly the second, large-scale sample of each texture modulates the first.
    tile_break: f32,
    _pad: vec2<f32>,
}

// The group is a shader def rather than a number: Bevy moved the material bind group to 3 in this
// version, and a hard-coded 2 compiles fine and then fails at pipeline build time with "not
// available in the pipeline layout". Bindings from 100 up are the range an extension owns; the base
// material has 0..99.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> rules: GroundRules;

// One pair per layer. Unrolled rather than an array, because WGSL cannot index a list of textures
// with a loop variable — and four `if`s are honest about what the hardware does anyway.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var texture_0: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var sampler_0: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var texture_1: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var sampler_1: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var texture_2: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(106) var sampler_2: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(107) var texture_3: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(108) var sampler_3: sampler;

/// How sharply the triplanar blend favours the plane a surface faces.
///
/// Six is high enough that flat ground is the Y plane and nothing else — which is what lets the
/// other two branches be skipped on most of the map — and low enough that a ravine wall crosses
/// over the couple of degrees around 45° rather than switching.
const SHARPNESS: f32 = 6.0;

/// Below this a projection contributes less than the eye can see and is not sampled.
const NEGLIGIBLE: f32 = 0.002;

/// A band with a soft edge, exactly as `Band::weight` computes it in Rust.
///
/// `low` and `high` rather than `from` and `to`, because `from` is a reserved word in WGSL — it
/// compiles in Rust, reads the same in both files, and fails at pipeline build time with an error
/// nothing but running the game will show you.
fn band(x: f32, low: f32, high: f32, blend: f32) -> f32 {
    let width = max(blend, 1.0e-4);
    let rising = clamp((x - low) / width + 0.5, 0.0, 1.0);
    let falling = clamp((high - x) / width + 0.5, 0.0, 1.0);
    return min(rising * rising * (3.0 - 2.0 * rising), falling * falling * (3.0 - 2.0 * falling));
}

/// How much larger the tile break's second sample is than the first.
///
/// Deliberately not a whole multiple: at 7.3 the two repeats never line up again inside any view,
/// so the pattern the break exists to hide cannot come back at its own scale.
const BREAK_SCALE: f32 = 7.3;

/// One texture on one plane, with the large-scale modulation multiplied in.
///
/// Centred on one rather than a straight multiply. `a * b` would square the texture's variance and
/// turn the albedo to mud; `a * mix(1, 2b, strength)` leaves the average where it was and only
/// moves it about.
fn planar(
    tex: texture_2d<f32>,
    samp: sampler,
    uv: vec2<f32>,
    ddx: vec2<f32>,
    ddy: vec2<f32>,
) -> vec3<f32> {
    let a = textureSampleGrad(tex, samp, uv, ddx, ddy).rgb;
    if rules.tile_break <= 0.0 {
        return a;
    }
    let k = 1.0 / BREAK_SCALE;
    let b = textureSampleGrad(tex, samp, uv * k, ddx * k, ddy * k).rgb;
    return a * mix(vec3<f32>(1.0), b * 2.0, rules.tile_break);
}

/// One layer, sampled on whichever world planes the surface faces.
///
/// `dx` and `dy` are the screen-space derivatives of the *world position*, taken once in the
/// fragment's uniform control flow. Dividing them by the tile scale gives the derivative of each
/// plane's uv, which is what `textureSampleGrad` wants and what a plain `textureSample` would have
/// worked out for itself if it were allowed to run here.
fn triplanar(
    tex: texture_2d<f32>,
    samp: sampler,
    world: vec3<f32>,
    dx: vec3<f32>,
    dy: vec3<f32>,
    weights: vec3<f32>,
    tile_metres: f32,
) -> vec3<f32> {
    let k = 1.0 / max(tile_metres, 0.01);
    var total = vec3<f32>(0.0);
    if weights.y > NEGLIGIBLE {
        total += weights.y * planar(tex, samp, world.xz * k, dx.xz * k, dy.xz * k);
    }
    if weights.x > NEGLIGIBLE {
        total += weights.x * planar(tex, samp, world.zy * k, dx.zy * k, dy.zy * k);
    }
    if weights.z > NEGLIGIBLE {
        total += weights.z * planar(tex, samp, world.xy * k, dx.xy * k, dy.xy * k);
    }
    return total;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let world = in.world_position.xyz;
    let normal = normalize(in.world_normal);
    // Degrees from flat, which is what the bands are written in — see `Layer::slope`.
    let slope = degrees(acos(clamp(normal.y, -1.0, 1.0)));

    // Taken here, before any branch, because a derivative inside non-uniform control flow is
    // meaningless: the neighbouring pixels the hardware differences against may not have taken the
    // same path.
    let dx = dpdx(world);
    let dy = dpdy(world);

    // How much each world plane faces the camera-side of this surface. Normalised, so the three
    // always sum to one and a blend of them is an average rather than a brightening.
    let facing = pow(abs(normal), vec3<f32>(SHARPNESS));
    let planes = facing / max(facing.x + facing.y + facing.z, 1.0e-6);

    // Metres this point sits below the ground around it, interpolated across the triangle from the
    // three corners `ground_mesh` measured it at.
    let dip = in.uv_b.x;

    var weight = vec4<f32>(0.0);
    for (var i = 0u; i < rules.count; i = i + 1u) {
        let s = rules.slope[i];
        let h = rules.height[i];
        let d = rules.dip[i];
        weight[i] = band(slope, s.x, s.y, s.z)
            * band(world.y, h.x, h.y, h.z)
            * band(dip, d.x, d.y, d.z);
    }
    let total = weight.x + weight.y + weight.z + weight.w;

    var colour = vec3<f32>(0.0);
    var roughness = 0.0;
    // Unrolled because the texture bindings cannot be indexed. A layer at zero weight is skipped
    // entirely, which on most of the map is three of the four.
    if weight.x > NEGLIGIBLE {
        var c = rules.colour[0].rgb;
        if rules.slope[0].w > 0.5 {
            c = triplanar(texture_0, sampler_0, world, dx, dy, planes, rules.height[0].w);
        }
        colour += weight.x * c;
        roughness += weight.x * rules.colour[0].w;
    }
    if weight.y > NEGLIGIBLE {
        var c = rules.colour[1].rgb;
        if rules.slope[1].w > 0.5 {
            c = triplanar(texture_1, sampler_1, world, dx, dy, planes, rules.height[1].w);
        }
        colour += weight.y * c;
        roughness += weight.y * rules.colour[1].w;
    }
    if weight.z > NEGLIGIBLE {
        var c = rules.colour[2].rgb;
        if rules.slope[2].w > 0.5 {
            c = triplanar(texture_2, sampler_2, world, dx, dy, planes, rules.height[2].w);
        }
        colour += weight.z * c;
        roughness += weight.z * rules.colour[2].w;
    }
    if weight.w > NEGLIGIBLE {
        var c = rules.colour[3].rgb;
        if rules.slope[3].w > 0.5 {
            c = triplanar(texture_3, sampler_3, world, dx, dy, planes, rules.height[3].w);
        }
        colour += weight.w * c;
        roughness += weight.w * rules.colour[3].w;
    }

    // A point outside every band is possible and must not come out black: a gap should look like a
    // gap rather than like unlit ground, so it is painted in a colour nothing else in this game is.
    if total > 1.0e-4 {
        colour /= total;
        roughness /= total;
    } else {
        colour = vec3<f32>(0.5, 0.0, 0.5);
        roughness = 1.0;
    }

    pbr_input.material.base_color = vec4<f32>(colour, 1.0);
    pbr_input.material.perceptual_roughness = clamp(roughness, 0.05, 1.0);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
