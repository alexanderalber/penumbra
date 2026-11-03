// A field of hexagonal prisms side by side beside the lane of the synthwave stage, one per texel,
// drawn by geo.tex_instance with hex.obj. The prisms stand in a honeycomb fixed to the floor, so
// the field comes nearer with the floor; the lane stays free. Their heights come from noise fixed
// to the floor, so hills and valleys of prisms keep their shape while they come nearer, and they
// rise a little on every beat.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// Out: x, y, z of the prism's centre in rgb, its width in alpha (0 where none stands). A second
// node (sizes.wgsl) turns that into the prisms' scale. Render it 32 x 32: a column of texels is a
// column of the honeycomb across the floor, a row a place along it.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the prisms rise a little on its start
// @param presence float = 1 0..1 -- how much of the field stands: 1 all of it, towards 0 the prisms sink into the floor one after another, 0 none
// @param seed int = 1 0..1000 -- another number gives another field
// @param radius float = 0.35 0.1..1.5 -- a prism's size: metres from its middle to a corner
// @param fill float = 0.92 0.5..1 -- how much of its cell a prism fills: 1 side by side without a gap
// @param lowest float = 0.4 0.05..6 -- the lowest prisms, metres
// @param tallest float = 3.2 0.2..10 -- the tallest prisms, metres (the camera is 1.1 to 2.9 m up, the ceiling 4 m)
// @param hills float = 3 0.5..20 -- metres between two hills of the field: smaller is more broken
// @param pulse float = 0.06 0..0.5 -- how far a prism rises on the beat, as a share of its height
// @param lane float = 1.6 0..5 -- half the width of the free lane; no prism reaches into it
// @param near float = -2 -8..10 -- where the prisms leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

// Whether the floor stands under a thing `y` metres ahead: the lane picture's alpha, read as in
// or out. The floor breaks off in whole cells of one beat, so the test reads the middle of the
// thing's cell from one texel, without filtering. A thing on a gap's edge then stays in or out
// as it comes nearer, instead of flickering as a stub. A gap that is opening takes it at once.
fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}
// Metres a prism's base stands above the floor.
const LIFT: f32 = 0.02;

fn hash(i: vec2i, seed: u32) -> f32 {
    return f32(pn_hash(vec3u(bitcast<vec2u>(i), seed)).x) / 4294967296.0;
}

fn noise(p: vec2f, seed: u32) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let k = vec2i(i);
    let a = hash(k, seed);
    let b = hash(k + vec2i(1, 0), seed);
    let c = hash(k + vec2i(0, 1), seed);
    let d = hash(k + vec2i(1, 1), seed);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let size = vec2u(pn.resolution);
    let seed = u32(max(params.seed, 0));
    let r = max(params.radius, 0.05);
    // The honeycomb: columns 1.5 radii apart across, rows one hexagon's height apart along, every
    // second column half a row later.
    let dx = 1.5 * r;
    let dy = sqrt(3.0) * r;
    let column = i32(cell.x) - i32(size.x / 2u);
    let x = (f32(column) + 0.5) * dx;
    let shift = 0.5 * f32(u32(column + 1000) % 2u);
    // Rows fixed to the floor. Only the travel's fraction moves them, kept apart from the whole
    // count to stay exact.
    let travel = params.beats * step / dy;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / dy + part - shift);
    let k = first + f32(cell.y);
    let y = dy * (k - part + shift);
    if y > params.far {
        return vec4f(x, y, 0.0, 0.0);
    }
    // The prism's own place on the floor, the same while it comes nearer.
    let row = i32(whole + k);
    let ground = vec2f(x, (f32(row) + shift) * dy);
    // The lane where the prism stands: it keeps out of it, shrinking whole at its edge.
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let free = abs(x - lane.r) - r * (1.0 + abs(lane.g)) - params.lane;
    var whole_size = smoothstep(0.0, 0.6, free);
    // Heights from two octaves of noise fixed to the floor.
    let h = vec2f(ground.x, ground.y + f32(seed) * 37.0) / max(params.hills, 0.1);
    let n = clamp(0.7 * noise(h, seed) + 0.3 * noise(h * 2.3 + vec2f(5.1, 1.7), seed + 17u), 0.0, 1.0);
    let lo = min(params.lowest, params.tallest);
    var height = mix(lo, params.tallest, n * n);
    // A little taller on the start of each beat, rising quickly and settling over the beat.
    let b = clamp(params.beat, 0.0, 1.0);
    let kick = smoothstep(0.0, 0.08, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
    height *= 1.0 + params.pulse * kick;
    // Presence: each prism has its own share of it, so as presence falls the prisms sink into
    // the floor one after another. Where the floor has a gap, no prism stands.
    let order = f32(pn_hash(vec3u(bitcast<u32>(row), bitcast<u32>(column), seed + 5u)).x) / 4294967296.0;
    let own = clamp((params.presence * 2.0 - order) * 2.0, 0.0, 1.0);
    let full = height;
    height *= own * own * (3.0 - 2.0 * own);
    if wired && !on_floor(y, step) {
        return vec4f(x, y, 0.0, 0.0);
    }
    let wide = 2.0 * r * clamp(params.fill, 0.1, 1.0) * whole_size;
    height *= whole_size;
    // A prism that has sunk below half a metre (a lower one: below 0.9 of its height) is left out
    // rather than standing as a stub. Every prism stands `LIFT` above the floor, so its base does
    // not flicker against the floor's lines.
    if height < min(0.5, 0.9 * full) || wide < 0.01 {
        return vec4f(x, y, 0.0, 0.0);
    }
    return vec4f(x, y, 0.5 * height + LIFT, wide);
}
