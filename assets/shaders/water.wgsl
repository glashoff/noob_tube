// The sea, drawn from one number per vertex.
//
// **Everything here comes out of the depth of the water at this pixel** — the metres between the
// surface and the ground beneath it, measured when the mesh was built and interpolated across the
// triangle. The usual way to have that in a shader is a depth pre-pass: render the scene's depth,
// then reconstruct the distance from the water plane to whatever is behind it. None of that is
// needed, because the client holds the height field and a subtraction is exact. One attribute, and
// out of it come:
//
//   * the colour, clear green-blue in the shallows and dark blue where it is deep
//   * the opacity, since you can see the ground through 20 cm of water and not through three metres
//   * the foam, a band that hugs the shoreline wherever the ground comes up to meet the surface
//   * the shoreline itself: a fragment with no water over it fades to nothing, so the waterline
//     lands exactly where the ground crosses the level rather than on the nearest grid cell
//
// The waves are arithmetic rather than a texture, and the surface never moves: this is a normal
// perturbation and a foam mask, not displacement. The mesh's own vertices stay flat at the water
// level, so a shore that a stroke has just moved is still a shore this frame.
//
// Ported from `../webgame`, `client/src/waterMaterial.ts`, which is where the numbers were found.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{alpha_discard, apply_pbr_lighting, main_pass_post_lighting_processing},
    mesh_view_bindings::{globals, view},
}

// The look. Linear colours, converted once on the CPU — see `WaterLook` in `client/src/water.rs`,
// which is the only other place this layout is written down.
struct WaterLook {
    // rgb: shallow water. w: metres of depth over which colour and opacity reach the deep values.
    shallow: vec4<f32>,
    // rgb: deep water. w: the width of the foam band, in metres of depth.
    deep: vec4<f32>,
    // rgb: foam. w: metres per wave — larger is a longer, calmer swell.
    foam: vec4<f32>,
    // x: opacity at the shore. y: in the deep. z: how fast the waves travel. w: how far a wave
    // tilts the surface normal.
    numbers: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> look: WaterLook;

// How far out the two fine octaves are worth evaluating, in metres.
//
// A quality decision before it is a cost one: past a hundred metres their wavelength is smaller
// than a pixel, so keeping them trades a still surface for a shimmering one.
const DETAIL_FROM: f32 = 40.0;
const DETAIL_TO: f32 = 140.0;

// How glossy water is, and how glossy foam is. Foam is a mess of bubbles and scatters in every
// direction, which is the whole of the difference.
const WATER_GLOSS: f32 = 0.04;
const FOAM_GLOSS: f32 = 0.75;

/// The ripple field: height in `x`, gradient in `yz`, one evaluation per fragment.
///
/// Four octaves close to the camera and two further out. **The gradient is analytic** — the
/// derivative of a sine is a cosine of the same argument — so a single pass yields both the wave
/// height, which the foam reads, and the surface normal, which the specular does. The obvious
/// version samples the height three times and takes finite differences: four evaluations instead of
/// one, for the same picture.
///
/// The directions are deliberately not axis-aligned. Waves running along x and z interfere in a
/// grid, and a sea with a grid in it reads as a bedsheet.
fn ripples(p: vec2<f32>, t: f32, detail: f32) -> vec3<f32> {
    let scale = look.foam.w;
    let speed = look.numbers.z;

    let k0 = vec2<f32>(0.87, 0.49) / scale;
    let a0 = dot(p, k0) + t * speed;
    var acc = vec3<f32>(sin(a0), cos(a0) * k0);

    let k1 = vec2<f32>(-0.42, 0.91) / (scale * 0.63);
    let a1 = dot(p, k1) + t * speed * 1.31;
    acc.x += 0.8 * sin(a1);
    acc = vec3<f32>(acc.x, acc.yz + 0.8 * cos(a1) * k1);

    if detail > 0.01 {
        let k2 = vec2<f32>(0.71, -0.70) / (scale * 0.27);
        let a2 = dot(p, k2) + t * speed * 1.87;
        acc.x += detail * 0.5 * sin(a2);
        acc = vec3<f32>(acc.x, acc.yz + detail * 0.5 * cos(a2) * k2);

        let k3 = vec2<f32>(0.13, 0.99) / (scale * 0.11);
        let a3 = dot(p, k3) + t * speed * 2.53;
        acc.x += detail * 0.35 * sin(a3);
        acc = vec3<f32>(acc.x, acc.yz + detail * 0.35 * cos(a3) * k3);
    }
    return acc;
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let world = in.world_position.xyz;
    // Metres of water over this pixel, interpolated from the three corners the mesh measured it at.
    // Negative on the dry side of a quad that straddles the waterline.
    let deep = in.uv_b.x;

    let near = 1.0 - smoothstep(DETAIL_FROM, DETAIL_TO, distance(view.world_position, world));
    let wave = ripples(world.xz, globals.time, near);

    let fade = clamp(deep / max(look.shallow.w, 1.0e-4), 0.0, 1.0);
    let colour = mix(look.shallow.rgb, look.deep.rgb, fade);

    // Foam: a band in the shallows, chewed up by the same wave field so that it moves with the
    // water instead of sitting on the sand as a painted outline.
    let shore = 1.0 - smoothstep(0.0, look.deep.w, deep);
    let foam = clamp(shore * smoothstep(0.15, 0.85, shore + wave.x * 0.25), 0.0, 1.0);

    // The dry half of a quad that straddles the waterline, cut on the *interpolated* depth — which
    // is what makes the shore follow the ground rather than the mesh resolution.
    //
    // Faded to zero alpha rather than discarded, deliberately. A shader containing a `discard` is
    // flagged by the driver as one whose coverage is unknown: it loses early-Z, it can lose
    // framebuffer compression, and under MSAA some drivers escalate it to per-sample execution — a
    // cliff on exactly the surface that already covers half the screen. This one is blended anyway,
    // so zero alpha is the same picture and costs the lighting of a thin band along the shore.
    let cut = step(1.0e-4, deep);
    let opacity = clamp(mix(look.numbers.x, look.numbers.y, fade) + foam * 0.5, 0.0, 1.0) * cut;

    pbr_input.material.base_color = vec4<f32>(mix(colour, look.foam.rgb, foam), opacity);
    pbr_input.material.perceptual_roughness = mix(WATER_GLOSS, FOAM_GLOSS, shore);

    // The wave normal, straight from the gradient computed above, and flattened as the water
    // shallows out: a ripple as tall as the water is deep reads as a rock, and it is exactly where
    // the surface meets the ground that the error would show.
    let amount = look.numbers.w * clamp(deep / 0.6, 0.0, 1.0);
    let tilt = normalize(vec3<f32>(-wave.y * amount, 1.0, -wave.z * amount));
    // Seen from below — a player under the surface — the sheet faces the other way, and a normal
    // pointing up would light the underside as though the sun were beneath it.
    pbr_input.N = select(-tilt, tilt, is_front);

    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
