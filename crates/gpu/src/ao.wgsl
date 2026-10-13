// Ambient occlusion for the render node (render.rs): from the prepass's positions and normals,
// per pixel how much of the hemisphere above the surface the geometry around it hides, then a
// blur that removes the sampling pattern's noise.
//
// The estimate: points within `radius` above the surface, spread over the hemisphere by the
// cosine (as ambient light arrives), each projected into the picture. A point hides its direction
// when the surface seen there lies in front of it (the point is inside or behind something) and
// rises above the pixel's own tangent plane (so a flat surface, seen at however steep an angle,
// never hides itself), fading out from one to two radii away (a wall far in front hides nothing).
// The share of hidden points is the share of ambient light that does not arrive: a point in a
// right-angled crease loses about half.
// The pattern turns from pixel to pixel over a 4x4 cell, which the blur averages over, keeping to
// pixels of about the same distance and facing so edges stay sharp.

struct Ao {
    clip: mat4x4f,
    view_z: vec4f,
    size: vec2f,
    radius: f32,
    strength: f32,
};

@group(0) @binding(0) var<uniform> ao: Ao;
// xyz: world position; w: distance along the view, 0 where nothing opaque was drawn.
@group(0) @binding(1) var positions: texture_2d<f32>;
// The normal, turned to the viewer.
@group(0) @binding(2) var normals: texture_2d<f32>;
// The blur's input: the estimate.
@group(0) @binding(3) var estimate: texture_2d<f32>;

const SAMPLES: u32 = 16u;
const TAU: f32 = 6.28318531;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let uv = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(uv * 2.0 - 1.0, 0.0, 1.0);
}

fn load(t: texture_2d<f32>, p: vec2i) -> vec4f {
    return textureLoad(t, clamp(p, vec2i(0), vec2i(textureDimensions(t)) - 1), 0);
}

// The 4x4 Bayer matrix, 0..15: neighbours far apart in the order.
fn bayer(p: vec2i) -> u32 {
    let x = vec2u(p & vec2i(3));
    let lo = ((x.x ^ x.y) & 1u) * 2u + (x.y & 1u);
    let hi = (((x.x ^ x.y) >> 1u) & 1u) * 2u + ((x.y >> 1u) & 1u);
    return lo * 4u + hi;
}

// Visibility in red (1 open, 0 hidden), the distance along the view in green (0: nothing).
@fragment
fn fs_estimate(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let px = vec2i(floor(pos.xy));
    let here = load(positions, px);
    if here.w <= 0.0 {
        return vec4f(1.0, 0.0, 0.0, 1.0);
    }
    let p = here.xyz;
    let n = normalize(load(normals, px).xyz);
    let a = select(vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), abs(n.x) > 0.8);
    let t = normalize(cross(a, n));
    let b = cross(n, t);
    let cell = f32(bayer(px)) / 16.0;
    let r = max(ao.radius, 1e-6);
    var hidden = 0.0;
    for (var i = 0u; i < SAMPLES; i++) {
        // Cosine-weighted directions, a golden angle apart, the whole set turned per pixel;
        // lengths the same for every pixel (turned with the directions, so a quarter turn of the
        // scene gives the same estimate), in bit-reversed order, more of them near the point.
        let u = (f32(i) + 0.5) / f32(SAMPLES);
        let phi = f32(i) * 2.39996323 + cell * TAU;
        let s = sqrt(u);
        let d = t * (s * cos(phi)) + b * (s * sin(phi)) + n * sqrt(1.0 - u);
        let k = (f32(reverseBits(i) >> 28u) + 0.5) / 16.0;
        let q = p + d * (r * mix(0.1, 1.0, k * k));
        let c = ao.clip * vec4f(q, 1.0);
        if c.w <= 1e-9 {
            continue;
        }
        let ndc = c.xy / c.w;
        if abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0 {
            continue;
        }
        let seen = load(positions, vec2i(floor((ndc * vec2f(0.5, -0.5) + 0.5) * ao.size)));
        if seen.w <= 0.0 {
            continue;
        }
        // In front of the point by more than a sliver of the radius.
        let depth = dot(ao.view_z, vec4f(q, 1.0));
        if seen.w >= depth - 0.02 * r {
            continue;
        }
        let v = seen.xyz - p;
        let dist = length(v);
        // Rising less than about six degrees counts as flat (a sphere's facets, rounding).
        if dist < 1e-6 * r || dot(v, n) < 0.1 * dist {
            continue;
        }
        hidden += 1.0 - smoothstep(r, 2.0 * r, dist);
    }
    // The strength scales what is hidden, so below 1 nothing goes fully dark.
    let lost = hidden / f32(SAMPLES) * max(ao.strength, 0.0);
    return vec4f(clamp(1.0 - lost, 0.0, 1.0), here.w, 0.0, 1.0);
}

// The estimate averaged over the 4x4 cell around the pixel, from pixels of about the same
// distance and facing. Centred: five pixels each way, the outer two at half weight, which still
// counts every turn of the pattern once.
@fragment
fn fs_blur(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let px = vec2i(floor(pos.xy));
    let here = load(estimate, px);
    if here.g <= 0.0 {
        return vec4f(1.0, 0.0, 0.0, 1.0);
    }
    let n = load(normals, px).xyz;
    let near = 0.02 * here.g + 0.05 * ao.radius;
    var sum = 0.0;
    var weight = 0.0;
    for (var y = -2; y <= 2; y++) {
        for (var x = -2; x <= 2; x++) {
            let at = px + vec2i(x, y);
            let e = load(estimate, at);
            let same = e.g > 0.0 && abs(e.g - here.g) < near && dot(load(normals, at).xyz, n) > 0.8;
            let w = select(0.0, select(1.0, 0.5, abs(x) == 2) * select(1.0, 0.5, abs(y) == 2), same);
            sum += e.r * w;
            weight += w;
        }
    }
    return vec4f(select(here.r, sum / weight, weight > 0.0), here.g, 0.0, 1.0);
}
