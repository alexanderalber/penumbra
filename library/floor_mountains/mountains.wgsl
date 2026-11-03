// Mountain ranges on the floor of the synthwave stage, one peak per texel, drawn by
// geo.tex_instance with a pyramid. A column of texels is a place across (x), a row a place along
// the floor (y). The floor moves towards the camera: `beats` is the distance travelled in beats.
// The heights come from noise fixed to the floor, so a ridge keeps its shape while it comes
// nearer, and another `seed` gives another range. The peaks stay off the stage's lane: low at
// its edge, rising to full height over `foot` metres.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// Out: x, y, z of the peak's centre in rgb, its width in alpha (0 where no peak stands). A
// second node (sizes.wgsl) turns that into the peaks' scale. Render it at most 32 x 64.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the grid's tempo, smoothed and added up
// @param presence float = 1 0..1 -- how much of the range stands: 1 all of it, towards 0 the peaks sink back into the floor one after another, 0 none
// @param seed int = 1 0..1000 -- another number gives another range
// @param tallest float = 2.6 0.2..6 -- height of the tallest peaks, in metres (the camera is 2 m up, the ceiling 4 m)
// @param base float = 1.8 0.2..5 -- width of a peak's foot across, in metres
// @param spacing float = 1 0.2..4 -- metres between two peaks, along and across
// @param peaks float = 3 0.5..20 -- metres between two summits of a ridge: smaller is more jagged
// @param foot float = 1.5 0..10 -- metres beside the lane over which the range rises to full height
// @param valleys float = 0.3 0..0.9 -- peaks lower than this share of `tallest` are left out, so the floor shows between the ridges
// @param lane float = 1.6 0..5 -- half the width of the free lane; no peak's foot reaches into it
// @param spread float = 18 1..60 -- width of the range across, both sides together
// @param near float = -1 -8..10 -- where the peaks leave, behind the camera
// @param far float = 12 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres a peak's base stands above the floor.
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

// Ridged noise: sharp crests where the noise crosses its middle, three octaves.
fn ridges(p: vec2f, seed: u32) -> f32 {
    var sum = 0.0;
    var weight = 0.0;
    var amp = 1.0;
    var q = p;
    for (var o = 0u; o < 3u; o++) {
        let n = 1.0 - abs(2.0 * noise(q, seed + o * 101u) - 1.0);
        sum += amp * n * n;
        weight += amp;
        amp *= 0.45;
        q = q * 2.03 + vec2f(17.1, 5.3);
    }
    return sum / weight;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = params.beats;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let size = vec2f(pn.resolution);
    let seed = u32(max(params.seed, 0));
    let span = max(params.far - params.near, 0.1);
    // Rows and columns in use: as many as `spacing` allows, at most the picture's size. The rows
    // fill the span exactly, so the range wraps round without a seam.
    let rows = clamp(floor(span / params.spacing), 1.0, size.y);
    let cols = clamp(floor(params.spread / params.spacing), 1.0, size.x);
    if f32(cell.y) >= rows || f32(cell.x) >= cols {
        return vec4f(0.0);
    }
    let along = span / rows;
    let across = params.spread / cols;
    let ahead = f32(cell.y) * along;
    let travelled = c * step + ahead;
    let lap = floor(travelled / span);
    // Three random numbers per peak and lap: its place within its cell, its size and its turn
    // in sinking.
    let r = vec3f(pn_hash(vec3u(cell, u32(max(lap, 0.0)) ^ (seed * 7919u)))) / 4294967296.0;
    let y = params.far - (travelled - lap * span) + (r.x - 0.5) * 0.4 * along;
    let x = -0.5 * params.spread + (f32(cell.x) + 0.5) * across + (r.y - 0.5) * 0.5 * across;
    // The place on the floor, which stays the same while the peak comes nearer.
    let ground = lap * span - ahead;
    let w = params.base * mix(0.8, 1.15, r.z);
    // The lane where the peak stands, and the room its foot leaves beside it. A turning lane runs
    // at a slant across the foot, so the slope widens the margin.
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let half = 0.5 * w + 0.5 * abs(lane.g) * w * 1.6;
    let free = abs(x - lane.r) - half - params.lane;
    if free <= 0.0 {
        return vec4f(x, y, 0.0, 0.0);
    }
    let rise = smoothstep(0.0, max(params.foot, 0.01), free);
    let crest = ridges(vec2f(x, ground) / max(params.peaks, 0.1), seed);
    let share = rise * crest * mix(0.8, 1.0, r.x);
    // Peaks fade out towards `valleys` instead of cutting off there: the lane swings, so a peak
    // at its edge would otherwise pop in and out as its share crosses the line.
    let keep = smoothstep(params.valleys, params.valleys + 0.15, share);
    var height = params.tallest * share * keep;
    // Presence: each peak has its own share of it, so as presence falls the peaks sink back into
    // the floor one after another. At 0.5 about half the range's height is left.
    let order = fract(r.z * 61.0 + r.y * 11.0);
    let own = clamp(params.presence * 2.0 - order, 0.0, 1.0);
    height *= own * own * (3.0 - 2.0 * own);
    // Where the floor has a gap, the peaks go with it.
    height *= clamp(lane.a, 0.0, 1.0);
    // A peak under 3 cm is left out, and every peak stands `LIFT` above the floor, so its base does
    // not flicker against the floor's lines.
    if height < 0.03 {
        return vec4f(x, y, 0.0, 0.0);
    }
    return vec4f(x, y, 0.5 * height + LIFT, w);
}
