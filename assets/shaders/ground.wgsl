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
// **Two textures a layer, not five.** Colour is one of them; the other packs the tangent normal's
// x and y, a roughness and a height into the four channels of a single image, baked offline by
// `tools/bake_ground_maps`. Read as three separate maps they would have cost three times the
// fetches of colour on a shader that already samples nine times for one layer on a slope. Packed,
// they cost one more sample wherever colour takes one — and the normal is the difference between
// ground that is a photograph laid flat and ground that answers the sun.
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
    // Hollow band in metres below the surroundings: from, to, blend. w = 1 once the layer's
    // packed detail texture has loaded — not merely once it has been asked for, because an image
    // still loading is bound as white, and white unpacks to a normal lying on its side.
    dip: array<vec4<f32>, 4>,
    // How many of the four rows are real. The struct's tail is rounded up to sixteen bytes by both
    // WGSL and `ShaderType`, so this needs no padding written after it — and a `vec3` pad would
    // have added twelve bytes *before* itself to reach its own alignment.
    count: u32,
}

// The group is a shader def rather than a number: Bevy moved the material bind group to 3 in this
// version, and a hard-coded 2 compiles fine and then fails at pipeline build time with "not
// available in the pipeline layout". Bindings from 100 up are the range an extension owns; the base
// material has 0..99.
@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> rules: GroundRules;

// One array texture for every layer's colour map, one for every layer's packed detail, indexed by
// the layer slot.
//
// **Arrays rather than a binding per layer, because sixteen is the ceiling.** A WebGPU fragment
// stage may sample sixteen textures — the figure Chrome reports whatever the hardware underneath —
// and Bevy's PBR pass has spent most of them before this file is reached. Eight of our own took the
// total to eighteen: the pipeline was refused outright, the opaque pass failed with it, and the
// browser client quit. Two cost two.
//
// It is also what lets the fragment entry point below be a loop rather than four copies of itself.
// A list of separate bindings cannot be indexed by anything the shader works out; an array can.
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var colours: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var ground_sampler: sampler;

// The packed detail maps: rg = tangent normal xy, b = roughness, a = height. No sampler of its own
// — a layer's two textures are read at the same uv with the same repeat and the same anisotropy, so
// one sampler serves both, and samplers have a ceiling of sixteen as well.
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var details: texture_2d_array<f32>;

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

/// Two independent values per lattice cell, and no `sin` in it.
///
/// Dave Hoskins' `hash22`. The obvious `fract(sin(dot(...)))` was good enough for noise measured in
/// tens of metres, but this is asked about lattice cells that run into the hundreds across the map,
/// and the argument to `sin` then runs into the hundred-thousands — where single precision has
/// nothing left and different hardware disagrees. `fract` early keeps every term small.
fn hash2(at: vec2<f32>) -> vec2<f32> {
    var p = fract(vec3<f32>(at.x, at.y, at.x) * vec3<f32>(0.1031, 0.1030, 0.0973));
    p += dot(p, p.yzx + 33.33);
    return fract((p.xx + p.yz) * p.zy);
}

/// One texture on one plane, sampled so that it does not repeat.
///
/// **Stochastic tiling**, after Heitz and Neyret. A texture laid down every four metres repeats,
/// and from far enough away to see many repeats at once the eye reads the repetition rather than
/// the material — a long rock wall turns into wallpaper. Nothing multiplied over the top fixes
/// that: the pattern is repeated *structure*, and a slow change in brightness cannot cancel
/// structure. Measured on this very texture at the mip a two-hundred-metre wall lands on, a
/// large-scale modulation left the grid exactly where it was.
///
/// So the repetition is removed instead of masked. The plane is covered with a triangular lattice;
/// each cell shifts the texture by its own random offset, and a point is blended from the three
/// cells whose corners surround it. The offsets are random, the lattice is not periodic in the
/// texture's own frame, and the seams between cells fall inside the blend.
///
/// The blend is **variance preserving**, and that is not a nicety. Averaging three samples of a
/// noisy texture flattens it toward its own mean — the flatter the more evenly the three are
/// weighted — so a plain average would leave soft patches wherever a point sat in the middle of a
/// triangle. Dividing the deviation by the length of the weight vector puts the contrast back
/// exactly. `mean` is the layer's average colour, which the uniform already carries.
///
/// Three samples where a plain lookup takes one. The tile break it replaces took two and did not
/// work, so on flat ground this is one extra fetch, and on the wall it was written for it is two.
/// The three cells a point is blended from, and how much each counts.
struct Cell {
    weights: vec3<f32>,
    corners: mat3x2<f32>,
}

/// Which three lattice cells surround this uv.
///
/// Split out so that a layer's colour and its packed detail are shifted by the *same* offsets. The
/// offsets are a function of the cell alone, so two textures read at one uv agree by construction —
/// and they have to, because a normal fetched from a different part of the texture than the colour
/// beside it is a surface lit as though it were somewhere else.
fn lattice(uv: vec2<f32>) -> Cell {
    // The lattice, skewed so that its cells are equilateral triangles rather than right ones.
    let scaled = uv * 3.464;
    let skewed = vec2<f32>(scaled.x - 0.57735027 * scaled.y, 1.15470054 * scaled.y);
    let cell = floor(skewed);
    let f = skewed - cell;
    // Which of the two triangles in this rhombus the point is in, and the barycentric weights of
    // whichever three corners those are.
    let z = 1.0 - f.x - f.y;
    var out: Cell;
    if z > 0.0 {
        out.weights = vec3<f32>(z, f.y, f.x);
        out.corners = mat3x2<f32>(cell, cell + vec2<f32>(0.0, 1.0), cell + vec2<f32>(1.0, 0.0));
    } else {
        out.weights = vec3<f32>(-z, 1.0 - f.y, 1.0 - f.x);
        out.corners = mat3x2<f32>(
            cell + vec2<f32>(1.0, 1.0),
            cell + vec2<f32>(1.0, 0.0),
            cell + vec2<f32>(0.0, 1.0),
        );
    }
    return out;
}

fn planar(
    tex: texture_2d_array<f32>,
    samp: sampler,
    layer: i32,
    uv: vec2<f32>,
    ddx: vec2<f32>,
    ddy: vec2<f32>,
    mean: vec3<f32>,
) -> vec3<f32> {
    let at = lattice(uv);
    let weights = at.weights;
    let corners = at.corners;
    // The gradients are the caller's and are the same for all three: the offsets are translations,
    // so every sample covers the same footprint and wants the same mip level.
    let a = textureSampleGrad(tex, samp, uv + hash2(corners[0]), layer, ddx, ddy).rgb;
    let b = textureSampleGrad(tex, samp, uv + hash2(corners[1]), layer, ddx, ddy).rgb;
    let c = textureSampleGrad(tex, samp, uv + hash2(corners[2]), layer, ddx, ddy).rgb;
    let mixed = weights.x * a + weights.y * b + weights.z * c;
    // Clamped at zero: putting the contrast back amplifies the deviation by up to √3, which on the
    // darkest texels of a dark texture reaches past black.
    return max(vec3<f32>(0.0), (mixed - mean) / length(weights) + mean);
}

/// One layer, sampled on whichever world planes the surface faces.
///
/// `dx` and `dy` are the screen-space derivatives of the *world position*, taken once in the
/// fragment's uniform control flow. Dividing them by the tile scale gives the derivative of each
/// plane's uv, which is what `textureSampleGrad` wants and what a plain `textureSample` would have
/// worked out for itself if it were allowed to run here.
fn triplanar(
    tex: texture_2d_array<f32>,
    samp: sampler,
    layer: i32,
    world: vec3<f32>,
    dx: vec3<f32>,
    dy: vec3<f32>,
    weights: vec3<f32>,
    tile_metres: f32,
    mean: vec3<f32>,
) -> vec3<f32> {
    let k = 1.0 / max(tile_metres, 0.01);
    var total = vec3<f32>(0.0);
    if weights.y > NEGLIGIBLE {
        total += weights.y * planar(tex, samp, layer, world.xz * k, dx.xz * k, dy.xz * k, mean);
    }
    if weights.x > NEGLIGIBLE {
        total += weights.x * planar(tex, samp, layer, world.zy * k, dx.zy * k, dy.zy * k, mean);
    }
    if weights.z > NEGLIGIBLE {
        total += weights.z * planar(tex, samp, layer, world.xy * k, dx.xy * k, dy.xy * k, mean);
    }
    return total;
}

/// One packed texel on one plane, tiled the same way its colour map is.
///
/// The variance-preserving step `planar` ends with is deliberately absent. That step restores the
/// contrast of a *colour* histogram around a mean the uniform carries, and there is no mean here
/// to restore around: a normal is a direction, and stretching a direction away from an average
/// direction does not sharpen anything, it tilts it. Averaging three normals flattens them
/// slightly, which is the same thing the mip chain does one level up and is what a blend of three
/// overlapping patches of gravel should look like.
fn planar_detail(
    tex: texture_2d_array<f32>,
    samp: sampler,
    layer: i32,
    uv: vec2<f32>,
    ddx: vec2<f32>,
    ddy: vec2<f32>,
) -> vec4<f32> {
    let at = lattice(uv);
    let a = textureSampleGrad(tex, samp, uv + hash2(at.corners[0]), layer, ddx, ddy);
    let b = textureSampleGrad(tex, samp, uv + hash2(at.corners[1]), layer, ddx, ddy);
    let c = textureSampleGrad(tex, samp, uv + hash2(at.corners[2]), layer, ddx, ddy);
    return at.weights.x * a + at.weights.y * b + at.weights.z * c;
}

/// The tangent-space normal a packed texel holds, with the component that was not stored.
///
/// z is dropped at bake time because a unit vector in the upper hemisphere does not carry anything
/// in its third component — and the byte it frees is worth more as a height. `max` against zero
/// because the two that were stored are eight-bit and their squares can just exceed one, where the
/// square root would be a NaN that spreads through the blend and comes out as a black pixel.
fn tangent(xy: vec2<f32>) -> vec3<f32> {
    let n = xy * 2.0 - 1.0;
    return vec3<f32>(n, sqrt(max(0.0, 1.0 - dot(n, n))));
}

/// What one layer's packed map says about this point: which way it faces, and how rough it is.
struct Detail {
    normal: vec3<f32>,
    roughness: f32,
}

/// One layer's detail, sampled on whichever world planes the surface faces.
///
/// **A triplanar normal map needs no tangents, and that is why this can exist at all.** The usual
/// way to read one is through a TBN basis the mesh carries per vertex, which the ground has never
/// had: it is a height field with no uv set to build a tangent from. But a triplanar surface is
/// already being read in three known frames — the world planes — and a tangent normal sampled on
/// one of them can be rotated into world space by a swizzle, because the frame *is* a pair of
/// world axes. Three of those, blended by the weights that are already computed, and the mesh is
/// not asked for anything.
///
/// The blend is Golus' whiteout: each plane's tangent normal has the geometric normal's other two
/// components added into its xy before the swizzle, so a detail normal perturbs the surface it sits
/// on rather than replacing it. Without it a steep face reads its detail as though the face were
/// flat, and the ravine walls light like vertical ground.
fn triplanar_detail(
    tex: texture_2d_array<f32>,
    samp: sampler,
    layer: i32,
    world: vec3<f32>,
    dx: vec3<f32>,
    dy: vec3<f32>,
    weights: vec3<f32>,
    tile_metres: f32,
    geometric: vec3<f32>,
) -> Detail {
    let k = 1.0 / max(tile_metres, 0.01);
    let n = geometric;
    var out: Detail;
    out.normal = vec3<f32>(0.0);
    out.roughness = 0.0;
    // The same three branches, in the same order and on the same thresholds, as the colour beside
    // it: a plane that contributes too little to be worth a colour fetch is not worth a normal.
    if weights.y > NEGLIGIBLE {
        let p = planar_detail(tex, samp, layer, world.xz * k, dx.xz * k, dy.xz * k);
        let t = tangent(p.xy);
        let w = vec3<f32>(t.xy + n.xz, abs(t.z) * n.y);
        out.normal += weights.y * w.xzy;
        out.roughness += weights.y * p.z;
    }
    if weights.x > NEGLIGIBLE {
        let p = planar_detail(tex, samp, layer, world.zy * k, dx.zy * k, dy.zy * k);
        let t = tangent(p.xy);
        let w = vec3<f32>(t.xy + n.zy, abs(t.z) * n.x);
        out.normal += weights.x * w.zyx;
        out.roughness += weights.x * p.z;
    }
    if weights.z > NEGLIGIBLE {
        let p = planar_detail(tex, samp, layer, world.xy * k, dx.xy * k, dy.xy * k);
        let t = tangent(p.xy);
        let w = vec3<f32>(t.xy + n.xy, abs(t.z) * n.z);
        out.normal += weights.z * w.xyz;
        out.roughness += weights.z * p.z;
    }
    // A sum of three perturbed normals is not a unit vector and the lighting wants one. It cannot
    // be zero: every branch adds the geometric normal's own components, and the weights sum to one.
    out.normal = normalize(out.normal);
    return out;
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

    // **The shading normal is the interpolated one, everywhere.** There was a version of this that
    // swapped in the triangle's own normal on steep ground — free, since the screen-space
    // derivatives of the world position are two of its edges — and while the ground was a smooth
    // height field it was the only thing that gave a rock face any edges at all. It is not that any
    // more: `Terrain::relief_at` moves every drawn vertex, so the roughness is in the surface, and
    // faceting on top of it only draws the triangulation over the top of the shape. Rock is rough
    // because it is rough, not because of how it is lit.

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
    var shading = vec3<f32>(0.0);
    // One pass per layer, and it can be a loop because the maps are array textures: the slot is an
    // index the shader works out, where a separate binding per layer could only be named. It used
    // to be four copies of this body with the numbers written in. A layer at zero weight is skipped
    // entirely, which on most of the map is three of the four.
    for (var i = 0u; i < rules.count; i = i + 1u) {
        let w = weight[i];
        if w <= NEGLIGIBLE {
            continue;
        }
        let slot = i32(i);
        var c = rules.colour[i].rgb;
        if rules.slope[i].w > 0.5 {
            c = triplanar(
                colours, ground_sampler, slot, world, dx, dy, planes, rules.height[i].w,
                rules.colour[i].rgb,
            );
        }
        // Roughness from the map when there is one, and the layer's own number when there is
        // not: the same relation `Layer::colour` has to a colour map, where the constant is what
        // the ground wears until something better has loaded and is measured from it afterwards.
        var n = normal;
        var r = rules.colour[i].w;
        if rules.dip[i].w > 0.5 {
            let d = triplanar_detail(
                details, ground_sampler, slot, world, dx, dy, planes, rules.height[i].w, normal,
            );
            n = d.normal;
            r = d.roughness;
        }
        colour += w * c;
        roughness += w * r;
        shading += w * n;
    }

    // A point outside every band is possible and must not come out black: a gap should look like a
    // gap rather than like unlit ground, so it is painted in a colour nothing else in this game is.
    if total > 1.0e-4 {
        colour /= total;
        roughness /= total;
    } else {
        colour = vec3<f32>(0.5, 0.0, 0.5);
        roughness = 1.0;
        shading = normal;
    }

    pbr_input.material.base_color = vec4<f32>(colour, 1.0);
    pbr_input.material.perceptual_roughness = clamp(roughness, 0.05, 1.0);
    // The shading normal, and only the shading normal. `pbr_input.world_normal` stays the one the
    // mesh gave, because that is what shadow biasing is measured against and a per-texel normal
    // would make the bias jitter from pixel to pixel. It is the same split Bevy's own normal
    // mapping makes. No division by `total`: normalising is about to make the scale irrelevant,
    // and the sum cannot be zero — every term is a perturbation of the same geometric normal.
    pbr_input.N = normalize(shading);
    pbr_input.material.base_color =
        alpha_discard(pbr_input.material, pbr_input.material.base_color);

    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
    return out;
}
