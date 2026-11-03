// Waves beside the lane of the synthwave stage: a height field drawn as lines across the floor.
// A row of texels is one line (a strip of the floor at one distance ahead), a column a point
// along it, from the left edge to the right. The lines are fixed to the floor and pass the
// camera with it; the waves also roll towards the camera along the floor, `flow` metres per
// beat. Ridged noise fixed to the floor gives the waves' shape, so a crest keeps its shape while
// it comes nearer; another `seed` gives other waves. The lane stays flat: the height falls to 0
// within `lane` of its middle and rises again over `foot` metres beside it.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// Out: x, y, z of the point in rgb, its height above the floor in alpha (0 where flat or no
// line). segments.wgsl joins neighbouring points into segments. Render it 65 x 48: 64 segments
// per line, 48 lines.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param presence float = 1 0..1 -- how much of the waves stand: 1 all, towards 0 the lines flatten into the floor one after another, 0 none
// @param source choice(noise|levels) = noise -- noise: ridged noise, raised by the bass; levels: the same noise scaled by the music's levels, the bass beside the lane, the treble at the edges
// @param seed int = 1 0..1000 -- another number gives other waves
// @param tall float = 1.2 0.1..4 -- height of the highest crests, in metres (the camera is 1.5 to 2.9 m up)
// @param bass float = 0.6 0..1 -- share of the height that comes and goes with the bass (source noise)
// @param low float = 0 0..1 -- the bass level (energy_low of a chan.beat_grid)
// @param mid float = 0 0..1 -- the mid level (energy_mid)
// @param high float = 0 0..1 -- the treble level (energy_high)
// @param crests float = 2.5 0.3..20 -- metres between two crests: smaller is choppier
// @param flow float = 0.15 -2..2 -- metres per beat the waves roll towards the camera, on top of the floor's own motion
// @param spacing float = 0.3 0.1..2 -- metres between two lines
// @param spread float = 20 2..40 -- width of the field across, both sides together
// @param lane float = 1.6 0..5 -- half the width of the flat lane
// @param foot float = 1.5 0.05..10 -- metres beside the lane over which the waves rise to full height
// @param near float = -1 -8..10 -- where the lines leave, behind the camera
// @param far float = 13 1..56 -- where they appear
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
// Metres every line stands above the floor, so it does not flicker against the floor's lines.
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
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let points = max(u32(pn.resolution.x), 2u);
    let spacing = max(params.spacing, 0.05);
    // Line n stands at y = spacing * (n - travel): it passes the camera when the floor has moved
    // n lines. Only the travel's fraction moves the lines, so it is kept apart from the whole
    // count to stay exact.
    let travel = params.beats * step / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / spacing + part);
    let k = first + f32(cell.y);
    let y = spacing * (k - part);
    let x = params.spread * (f32(cell.x) / f32(points - 1u) - 0.5);
    if y > params.far {
        return vec4f(x, y, 0.0, 0.0);
    }
    let n = whole + k;
    let seed = u32(max(params.seed, 0));
    // The lane where the line runs; a slanting lane widens the flat part.
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let off = abs(x - lane.r) - params.lane * (1.0 + 0.5 * abs(lane.g));
    let rise = smoothstep(0.0, max(params.foot, 0.05), off);
    // The waves: noise fixed to the floor, rolling on by `flow`.
    let along = n * spacing + params.flow * params.beats;
    let crest = ridges(vec2f(x, along) / max(params.crests, 0.1), seed);
    var level = mix(1.0, clamp(params.low, 0.0, 1.0), clamp(params.bass, 0.0, 1.0));
    if params.source == source_levels {
        // Outwards from the lane: bass, then mids, then treble.
        let o = clamp(off / max(0.5 * params.spread - params.lane, 0.1), 0.0, 1.0);
        let lo = clamp(params.low, 0.0, 1.0);
        let mi = clamp(params.mid, 0.0, 1.0);
        let hi = clamp(params.high, 0.0, 1.0);
        let band = select(mix(mi, hi, 2.0 * o - 1.0), mix(lo, mi, 2.0 * o), o < 0.5);
        level = clamp(0.15 + 1.2 * band, 0.0, 1.2);
    }
    var height = params.tall * crest * crest * rise * level;
    // Presence: each line has its own share of it, so as presence falls the lines flatten into
    // the floor one after another. At 0.5 about half the height is left.
    let order = hash(vec2i(i32(n), 7), seed + 3u);
    let own = clamp(params.presence * 2.0 - order, 0.0, 1.0);
    height *= own * own * (3.0 - 2.0 * own);
    // Where the floor has a gap, the line lies flat.
    height *= select(0.0, 1.0, !wired || on_floor(y, step));
    return vec4f(x, y, height + LIFT, height);
}
