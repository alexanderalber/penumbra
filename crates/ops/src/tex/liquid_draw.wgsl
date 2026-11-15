// tex.liquid's drawing: the particles of `liquid.wgsl` to the pictures `out`, `foam`, `grain`
// and `surface`.
//
// The shape: every liquid particle adds a kernel to a field, at a "splat" resolution where the
// particles lie about 1.4 pixels apart. The kernel grows as the particle's smoothed density
// falls (up to 4 times the area), so a thin stream stays whole instead of breaking into dots;
// a fast particle's kernel stretches along its velocity. Each particle adds the same amount, so
// the field is about 1 inside the liquid. A Gaussian blur of 1.8 particle spacings smooths it,
// and the liquid is where the field passes a half, with an edge of about a pixel.
//
// Beside the field the particles add weights for averages of their foam and grain. Sums are
// fixed point in atomics: the same particles give the same pictures in any order.
//
// Sand and snow (material 2 and 3) add to the same field, so one edge goes round all of it.
// Their grain is a random number from the particle's place in the buffer. Sand adds its grain
// with a kernel smaller than a particle's spacing, so each grain shows; snow varies its kernel's
// weight by its grain, so its edge is lumpy. While sand or snow is in the liquid, a second set of
// sums gives each pixel its share of sand and of snow, and the shading mixes the materials'
// colours by them.
//
// Shading: the coverage with the solids counted as filled, blurred twice: a little for the
// distance to the surface (a light band under it), a lot at a low resolution for the depth
// (the bulk darker). Foam lightens the liquid near its surface.
//
// Per frame: `splat` (per particle), `resolve` (sums to the shape picture, and back to 0),
// `shape_x`, `shape_y`, `depth_down`, `depth_x`, `depth_y`, `surface_x`, `surface_y`, then
// `shade` for `out` and `foam_out`, `grain_out` and `surface_out` for those outputs. With sand
// or snow, `shares` after `resolve` turns the second sums into a picture that `surface_x` and
// `surface_y` pass on to `shade`.

// Particles of material 1 are liquid, 2 sand, 3 snow.
const LIQUID: u32 = 1u;
const SAND: u32 = 2u;
const SNOW: u32 = 3u;
// Sand's grain kernel, in particle spacings.
const SAND_GRAIN: f32 = 0.75;
// How much a snow particle's weight in the field varies with its grain: from 1 - this to 1 +
// this.
const SNOW_LUMPS: f32 = 0.6;
// How far the shading looks for the materials' shares around a pixel no particle reaches, in
// splat pixels.
const SHARE_REACH: i32 = 3;
// The smoothed density below which a particle's kernel grows, and the most it grows.
const MOST_STRETCH: f32 = 4.0;
// The kernel's radius in particle spacings, at least this many splat pixels.
const RADIUS: f32 = 1.5;
const LEAST_RADIUS: f32 = 1.5;
// The averages' radius in particle spacings.
const AVERAGE: f32 = 1.8;
// The field's threshold, and half the width of its soft edge.
const LEVEL: f32 = 0.5;
const SOFT: f32 = 0.06;
// The most a blur reaches, in pixels: what a frame costs at most.
const MOST_REACH: i32 = 48;
// The light band under the surface: from this many particle spacings into the liquid to this
// many, and how much of the light colour it takes.
const BAND_FROM: f32 = 2.14;
const BAND_TO: f32 = 0.36;
const BAND: f32 = 0.55;

struct D {
    splat: vec4u,  // splat picture width, height; particles in the buffer; 1 when obstacles are wired
    room: vec4f,   // splat pixels per picture height across, down; particle spacing in splat pixels; unused
    kernel: vec4f, // fast speed (heights per second); 1 while sand or snow is in the liquid; unused; fixed-point scale
    blur: vec4f,   // sigma of the shape blur and of the surface blur (splat pixels), of the depth blur (depth pixels); unused
    look: vec4f,   // particle spacing in output pixels; foam strength; unused; unused
    color: vec4f,  // the liquid's colour, straight alpha
    sand: vec4f,   // sand's colour
    snow: vec4f,   // snow's colour
}

struct P {
    pos: vec2f,
    vel: vec2f,
    c0: vec2f,
    c1: vec2f,
    f0: vec2f,
    f1: vec2f,
    rho: f32,
    sd: f32,
    jp: f32,
    mat: u32,
}

@group(2) @binding(0) var<uniform> d: D;
@group(2) @binding(1) var<storage, read> parts: array<P>;
// Per splat pixel: the field, the averages' weight, foam times weight, grain times weight.
@group(2) @binding(2) var<storage, read_write> acc: array<atomic<u32>>;
// Per splat pixel while sand or snow is in the liquid: the averages' weight of sand, of snow,
// of all particles, unused.
@group(2) @binding(3) var<storage, read_write> shares: array<atomic<u32>>;

fn finite(v: vec2f) -> bool {
    return all(v == v) && all(abs(v) < vec2f(1e30));
}

fn fixed(v: f32) -> u32 {
    return u32(clamp(v * d.kernel.w + 0.5, 0.0, 4294967040.0));
}

fn hash(x: u32) -> u32 {
    var h = x * 747796405u + 2891336453u;
    h = ((h >> ((h >> 28u) + 4u)) ^ h) * 277803737u;
    return (h >> 22u) ^ h;
}

fn granular() -> bool {
    return d.kernel.y > 0.5;
}

@compute @workgroup_size(64)
fn splat(@builtin(global_invocation_id) id: vec3u) {
    let i = id.x;
    if (i >= pn.size.x) {
        return;
    }
    let p = parts[i];
    if (p.mat == 0u || p.mat > SNOW || !finite(p.pos) || !finite(p.vel)) {
        return;
    }
    let liquid = p.mat == LIQUID;
    let size = vec2i(d.splat.xy);
    let c = p.pos * d.room.xy;
    let s = d.room.z;
    let rad = max(RADIUS * s, LEAST_RADIUS);
    // Liquid keeps a smoothed density for this; sand and snow take the last one.
    let ratio = select(p.rho / max(d.look.z, 1e-6), p.f1.x, liquid);
    let thin = clamp(1.0 / max(ratio, 1e-3), 1.0, MOST_STRETCH);
    let speed = length(p.vel);
    var dir = vec2f(1.0, 0.0);
    var a = rad * sqrt(thin);
    var b = a;
    if (speed > d.kernel.x) {
        dir = p.vel / speed;
        a = rad * thin;
        b = rad;
    }
    // Every particle adds s^2 * thin in all: about 1 per pixel inside the liquid at rest.
    var norm = 3.0 * s * s / (3.14159265 * rad * rad);
    let ra = max(AVERAGE * s, LEAST_RADIUS);
    var foam = 0.0;
    var grain = f32(hash(i ^ 0x5bd1e995u) >> 8u) / 16777216.0;
    if (liquid) {
        foam = clamp(p.f1.y, 0.0, 1.0);
        grain = clamp(p.jp, 0.0, 1.0);
    } else if (p.mat == SNOW) {
        norm *= 1.0 + SNOW_LUMPS * (2.0 * grain - 1.0);
    }
    // Sand's grain goes into the averages with a kernel of its own, smaller than its spacing.
    let rg = select(ra, max(SAND_GRAIN * s, 0.75), p.mat == SAND);
    let ext = vec2f(length(vec2f(a * dir.x, b * dir.y)), length(vec2f(a * dir.y, b * dir.x)));
    let reach = vec2i(ceil(min(max(ext, vec2f(ra)), vec2f(f32(MOST_REACH)))));
    let mixed = granular();
    let sand = select(0.0, 1.0, p.mat == SAND);
    let snow = select(0.0, 1.0, p.mat == SNOW);
    let centre = vec2i(floor(c));
    let lo = max(centre - reach, vec2i(0));
    let hi = min(centre + reach, size - vec2i(1));
    for (var y = lo.y; y <= hi.y; y++) {
        for (var x = lo.x; x <= hi.x; x++) {
            let e = vec2f(f32(x), f32(y)) + vec2f(0.5) - c;
            let u = dot(e, dir) / a;
            let w = (e.y * dir.x - e.x * dir.y) / b;
            let d2 = u * u + w * w;
            let r2 = dot(e, e) / (ra * ra);
            let k = 4u * (u32(y) * d.splat.x + u32(x));
            if (d2 < 1.0) {
                let f = norm * (1.0 - d2) * (1.0 - d2);
                atomicAdd(&acc[k], fixed(f));
                if (mixed) {
                    // The shares weigh like the field, so wherever the field is the shares are.
                    atomicAdd(&shares[k], fixed(f * sand));
                    atomicAdd(&shares[k + 1u], fixed(f * snow));
                    atomicAdd(&shares[k + 2u], fixed(f));
                }
            }
            if (r2 < 1.0) {
                let g = (1.0 - r2) * (1.0 - r2);
                let q2 = dot(e, e) / (rg * rg);
                if (q2 < 1.0) {
                    let q = (1.0 - q2) * (1.0 - q2);
                    atomicAdd(&acc[k + 1u], fixed(q));
                    atomicAdd(&acc[k + 2u], fixed(q * foam));
                    atomicAdd(&acc[k + 3u], fixed(q * grain));
                }
            }
        }
    }
}

// The second sums to (sand share, snow share, 0, 0), and back to 0 for the next frame. A pixel
// no particle reaches has a sand share of -1: the shading looks around it.
@compute @workgroup_size(8, 8, 1)
fn resolve_shares(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let k = 4u * (id.y * d.splat.x + id.x);
    let all = atomicLoad(&shares[k + 2u]);
    var share = vec2f(-1.0, 0.0);
    if (all > 0u) {
        share = vec2f(f32(atomicLoad(&shares[k])), f32(atomicLoad(&shares[k + 1u]))) / f32(all);
        share = clamp(share, vec2f(0.0), vec2f(1.0));
    }
    atomicStore(&shares[k], 0u);
    atomicStore(&shares[k + 1u], 0u);
    atomicStore(&shares[k + 2u], 0u);
    textureStore(pic_out, id.xy, vec4f(share, 0.0, 0.0));
}

// The sums to (field, solid, foam, grain), and the sums back to 0 for the next frame. Solid
// comes from the `obstacles` picture (pic0).
@compute @workgroup_size(8, 8, 1)
fn resolve(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let k = 4u * (id.y * d.splat.x + id.x);
    let field = f32(atomicLoad(&acc[k])) / d.kernel.w;
    let weight = atomicLoad(&acc[k + 1u]);
    var foam = 0.0;
    var grain = 0.0;
    if (weight > 0u) {
        foam = f32(atomicLoad(&acc[k + 2u])) / f32(weight);
        grain = f32(atomicLoad(&acc[k + 3u])) / f32(weight);
    }
    atomicStore(&acc[k], 0u);
    atomicStore(&acc[k + 1u], 0u);
    atomicStore(&acc[k + 2u], 0u);
    atomicStore(&acc[k + 3u], 0u);
    var solid = 0.0;
    if (d.splat.w != 0u) {
        let uv = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy);
        let l = pn_luma(textureSampleLevel(pic0, pn_clamp, uv, 0.0).rgb);
        // NaN stays empty; infinity is solid.
        if (l == l) {
            solid = smoothstep(0.45, 0.55, clamp(l, -1.0, 2.0));
        }
    }
    textureStore(pic_out, id.xy, vec4f(min(field, 60000.0), solid, clamp(foam, 0.0, 1.0), clamp(grain, 0.0, 1.0)));
}

fn gauss(i: i32, sigma: f32) -> f32 {
    let x = f32(i);
    return exp(-x * x / (2.0 * sigma * sigma));
}

fn reach_of(sigma: f32) -> i32 {
    return clamp(i32(ceil(3.0 * sigma)), 0, MOST_REACH);
}

fn load0(at: vec2i) -> vec4f {
    let size = vec2i(textureDimensions(pic0));
    return textureLoad(pic0, clamp(at, vec2i(0), size - vec2i(1)), 0);
}

// The field and the solid blurred along `axis`, the rest passed on.
fn blur_shape(at: vec2i, axis: vec2i) -> vec4f {
    let sigma = d.blur.x;
    let r = reach_of(sigma);
    var sum = vec2f(0.0);
    var total = 0.0;
    for (var i = -r; i <= r; i++) {
        let g = gauss(i, sigma);
        sum += g * load0(at + axis * i).rg;
        total += g;
    }
    return vec4f(sum / max(total, 1e-6), load0(at).ba);
}

@compute @workgroup_size(8, 8, 1)
fn shape_x(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    textureStore(pic_out, id.xy, blur_shape(vec2i(id.xy), vec2i(1, 0)));
}

// (field, solid, foam, grain): the field blurred and made up for the solid around it, the
// solid unblurred (from pic1).
@compute @workgroup_size(8, 8, 1)
fn shape_y(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let b = blur_shape(vec2i(id.xy), vec2i(0, 1));
    // Next to a solid part of the blur's reach holds no particles: the field there counts only
    // the liquid's share, so liquid on a shelf does not thin out towards it.
    let field = b.r / max(1.0 - b.g, 0.5);
    let solid = textureLoad(pic1, vec2i(id.xy), 0).g;
    textureStore(pic_out, id.xy, vec4f(field, solid, b.b, b.a));
}

// The silhouette: 0 outside, 1 inside, an edge of about a pixel.
fn silhouette(field: f32) -> f32 {
    return smoothstep(LEVEL - SOFT, LEVEL + SOFT, field);
}

// Liquid or solid: what has no surface between.
fn filled(t: vec4f) -> f32 {
    return max(silhouette(t.r), t.g);
}

// The filled coverage at the depth picture's size, from the shape picture (pic0).
@compute @workgroup_size(8, 8, 1)
fn depth_down(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let step = 0.25 / vec2f(pn.size.xy);
    let uv = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy);
    var sum = 0.0;
    for (var k = 0; k < 4; k++) {
        let o = vec2f(f32(k & 1) * 2.0 - 1.0, f32(k >> 1u) * 2.0 - 1.0) * step;
        sum += filled(textureSampleLevel(pic0, pn_clamp, uv + o, 0.0));
    }
    textureStore(pic_out, id.xy, vec4f(sum * 0.25, 0.0, 0.0, 1.0));
}

fn blur_red(at: vec2i, axis: vec2i, sigma: f32) -> f32 {
    let r = reach_of(sigma);
    var sum = 0.0;
    var total = 0.0;
    for (var i = -r; i <= r; i++) {
        let g = gauss(i, sigma);
        sum += g * load0(at + axis * i).r;
        total += g;
    }
    return sum / max(total, 1e-6);
}

@compute @workgroup_size(8, 8, 1)
fn depth_x(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    textureStore(pic_out, id.xy, vec4f(blur_red(vec2i(id.xy), vec2i(1, 0), d.blur.z), 0.0, 0.0, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn depth_y(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    textureStore(pic_out, id.xy, vec4f(blur_red(vec2i(id.xy), vec2i(0, 1), d.blur.z), 0.0, 0.0, 1.0));
}

// The filled coverage of the shape picture (pic0), blurred along x; with sand or snow their
// shares from pic1 beside it.
@compute @workgroup_size(8, 8, 1)
fn surface_x(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let sigma = d.blur.y;
    let r = reach_of(sigma);
    let at = vec2i(id.xy);
    var sum = 0.0;
    var total = 0.0;
    for (var i = -r; i <= r; i++) {
        let g = gauss(i, sigma);
        sum += g * filled(load0(at + vec2i(i, 0)));
        total += g;
    }
    var share = vec2f(0.0);
    if (granular()) {
        share = textureLoad(pic1, at, 0).rg;
    }
    textureStore(pic_out, id.xy, vec4f(sum / max(total, 1e-6), 0.0, share));
}

// (blurred coverage, depth 0 to 1, sand share, snow share): the coverage blurred along y, the
// depth from the depth picture (pic1), the shares passed on.
@compute @workgroup_size(8, 8, 1)
fn surface_y(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let b = blur_red(vec2i(id.xy), vec2i(0, 1), d.blur.y);
    let uv = (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy);
    let deep = smoothstep(0.5, 0.95, textureSampleLevel(pic1, pn_clamp, uv, 0.0).r);
    var share = vec2f(0.0);
    if (granular()) {
        share = load0(vec2i(id.xy)).ba;
    }
    textureStore(pic_out, id.xy, vec4f(b, deep, share));
}

fn uv_of(id: vec3u) -> vec2f {
    return (vec2f(id.xy) + vec2f(0.5)) / vec2f(pn.size.xy);
}

// The liquid's alpha at `uv` of the output, from the shape picture (pic0): no liquid inside a
// solid.
fn alpha_at(t: vec4f) -> f32 {
    return silhouette(t.r) * (1.0 - t.g);
}

// The distance into the liquid in output pixels (negative outside), and the outward normal,
// from the surface picture (pic1).
struct Surface {
    inside: f32,
    deep: f32,
    normal: vec2f,
}

fn surface_at(id: vec3u) -> Surface {
    let uv = uv_of(id);
    let px = 1.0 / vec2f(pn.size.xy);
    let here = textureSampleLevel(pic1, pn_clamp, uv, 0.0);
    let gx = 0.5 * (textureSampleLevel(pic1, pn_clamp, uv + vec2f(px.x, 0.0), 0.0).r
        - textureSampleLevel(pic1, pn_clamp, uv - vec2f(px.x, 0.0), 0.0).r);
    let gy = 0.5 * (textureSampleLevel(pic1, pn_clamp, uv + vec2f(0.0, px.y), 0.0).r
        - textureSampleLevel(pic1, pn_clamp, uv - vec2f(0.0, px.y), 0.0).r);
    let g = vec2f(gx, gy);
    let len = length(g);
    var s: Surface;
    s.inside = clamp((here.r - 0.5) / max(len, 0.02), -1000.0, 1000.0);
    s.deep = here.g;
    s.normal = select(vec2f(0.0), -g / len, len > 1e-6);
    return s;
}

// The shares of sand and snow at a pixel of the output, from the surface picture (pic1): the
// nearest splat pixel's, or the mean of those around it that particles reach.
fn shares_at(id: vec3u) -> vec2f {
    let size = vec2i(textureDimensions(pic1));
    let at = vec2i(uv_of(id) * vec2f(size));
    let here = textureLoad(pic1, clamp(at, vec2i(0), size - vec2i(1)), 0).ba;
    if (here.x >= 0.0) {
        return here;
    }
    var sum = vec3f(0.0);
    for (var j = -SHARE_REACH; j <= SHARE_REACH; j++) {
        for (var i = -SHARE_REACH; i <= SHARE_REACH; i++) {
            let s = textureLoad(pic1, clamp(at + vec2i(i, j), vec2i(0), size - vec2i(1)), 0).ba;
            if (s.x >= 0.0) {
                sum += vec3f(s, 1.0);
            }
        }
    }
    return select(vec2f(0.0), sum.xy / sum.z, sum.z > 0.0);
}

// `out`: the liquid in its colour, lighter in thin parts and under the surface, darker deep
// down, foam lighter still; premultiplied, transparent around it.
@compute @workgroup_size(8, 8, 1)
fn shade(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let t = textureSampleLevel(pic0, pn_clamp, uv_of(id), 0.0);
    let a = alpha_at(t) * clamp(d.color.a, 0.0, 1.0);
    if (!(a > 0.0)) {
        textureStore(pic_out, id.xy, vec4f(0.0));
        return;
    }
    let s = surface_at(id);
    let shallow = max(d.color.rgb, vec3f(0.0));
    let deep = 0.17 * shallow * shallow;
    let light = mix(shallow, vec3f(1.0), 0.55);
    let white = mix(shallow, vec3f(1.0), 0.85);
    let spacing = d.look.x;
    var c = mix(shallow, deep, s.deep);
    c = mix(c, light, smoothstep(BAND_FROM * spacing, BAND_TO * spacing, s.inside) * BAND);
    let foam = smoothstep(0.25, 0.8, t.b) * (1.0 - 0.85 * s.deep) * clamp(d.look.y, 0.0, 1.0);
    c = mix(c, white, foam);
    if (granular()) {
        let solid_share = shares_at(id);
        let share = clamp(solid_share, vec2f(0.0), vec2f(1.0));
        let solid = share.x + share.y;
        if (solid > 1e-4) {
            let band = smoothstep(BAND_FROM * spacing, BAND_TO * spacing, s.inside);
            let g = clamp(t.a, 0.0, 1.0);
            // Sand: matte, each grain lighter or darker, darker deep in a heap, a little lighter on top.
            let sand = max(d.sand.rgb, vec3f(0.0));
            var cs = sand * (0.55 + 0.9 * g);
            cs = mix(cs, cs * 0.55, s.deep);
            cs = mix(cs, min(cs * 1.3, vec3f(1.0)), band * 0.4);
            // Snow: lit at its surface, bluer and darker deep in a pile, a faint grain.
            let snow = max(d.snow.rgb, vec3f(0.0));
            var cn = snow * (0.93 + 0.14 * g);
            cn = mix(cn, cn * vec3f(0.55, 0.66, 0.88), s.deep * 0.85);
            cn = mix(cn, mix(snow, vec3f(1.0), 0.5), band * 0.6);
            let k = min(solid, 1.0);
            let both = (cs * share.x + cn * share.y) / solid;
            c = mix(c, both, k);
        }
    }
    textureStore(pic_out, id.xy, vec4f(c * a, a));
}

// `foam`: the foam the particles carry, 0 to 1, as white over transparent where the liquid is.
@compute @workgroup_size(8, 8, 1)
fn foam_out(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let t = textureSampleLevel(pic0, pn_clamp, uv_of(id), 0.0);
    let f = clamp(t.b, 0.0, 1.0) * alpha_at(t);
    textureStore(pic_out, id.xy, vec4f(f));
}

// `grain`: the particles' random numbers, averaged, as grey on the liquid's alpha.
@compute @workgroup_size(8, 8, 1)
fn grain_out(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let t = textureSampleLevel(pic0, pn_clamp, uv_of(id), 0.0);
    let a = alpha_at(t);
    textureStore(pic_out, id.xy, vec4f(vec3f(clamp(t.a, 0.0, 1.0) * a), a));
}

// `surface`: distance into the liquid in output pixels (negative outside), depth 0 to 1, and
// the outward normal (x, y down); solids count as filled.
@compute @workgroup_size(8, 8, 1)
fn surface_out(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let s = surface_at(id);
    textureStore(pic_out, id.xy, vec4f(s.inside, s.deep, s.normal));
}
