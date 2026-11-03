// Split hexagonal pillars beside the lane of the synthwave stage, drawn by geo.tex_instance with
// a prism. Each pillar is two parts: a base standing on the floor and a counterpart hanging from
// the ceiling, with a gap of open air between them. The pillars stand where floor_pillars puts
// its pillars: in lines beside the lane, `spacing` bars of travel apart, the second line on a side
// half a gap later.
// Every pillar has a gap of its own: its middle between 1.2 and 3 m up and 0.3 to 1 m tall, both
// from a draw per pillar. On each beat the gap opens a little. Towards presence 0 both parts sink
// back, the base into the floor and the counterpart into the ceiling.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// Render it 12 x 32. A column is a line of parts: columns 0 to 5 the bases, 6 to 11 the
// counterparts of the same pillars (left lines first, then right ones, `rows` per side, at most
// 3). A row is a pillar along its line.
// Out: x, y, z of the part's centre in rgb, its height in alpha (0 where no part stands). A
// second node (sizes.wgsl) turns that into the parts' scale.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the gaps open a little on its start
// @param presence float = 1 0..1 -- how many pillars stand: 1 all, towards 0 their parts sink back into floor and ceiling one pillar after another, 0 none
// @param spacing float = 1 0.25..8 -- bars of travel between two pillars of a line
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param ceiling float = 4 3.2..8 -- the ceiling's height, metres: the counterparts hang from it
// @param seed int = 1 0..1000 -- another seed gives every pillar another gap
// @param low float = 1.2 0.5..3 -- the lowest middle of a gap, metres
// @param high float = 3 1..3.5 -- the highest middle of a gap, metres
// @param narrow float = 0.3 0.05..2 -- the narrowest gap, metres
// @param wide float = 1 0.1..2 -- the widest gap, metres
// @param across float = 0.36 0.05..2 -- width of a pillar, in metres
// @param rows int = 2 1..3 -- lines of pillars on each side of the lane
// @param gap float = 1.2 0.3..5 -- metres between two lines on one side
// @param pulse float = 0.3 0..1 -- how far a gap opens on the beat, as a share of its height
// @param lane float = 1.6 0..5 -- half the width of the free lane; the nearest line stands just outside it
// @param near float = -2 -8..10 -- where the pillars leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres a base stands above the floor, and a counterpart hangs below the ceiling, so their ends
// do not flicker against the grids' lines.
const LIFT: f32 = 0.02;
// A part shorter than this, metres, is left out rather than drawn as a sliver.
const SHORTEST: f32 = 0.15;

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

// What a texel writes for a part that is left out.
fn none(x: f32, y: f32) -> vec4f {
    return vec4f(x, y, 0.0, 0.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    if cell.x >= 12u {
        return none(0.0, 0.0);
    }
    let top = cell.x >= 6u;
    let column = cell.x % 6u;
    let rows = u32(clamp(params.rows, 1, 3));
    let side = column / 3u;
    let row = column % 3u;
    if row >= rows {
        return none(0.0, 0.0);
    }
    // Metres between two pillars of a line, and where the floor stands in pillars: pillar n
    // stands at y = gap_y * (n - travel), so it passes y = 0 where the bar count is a multiple
    // of `spacing`. Only the travel's fraction moves the pillars, so it is kept apart from the
    // whole count to stay exact.
    let bpb = max(params.beats_per_bar, 1.0);
    let spacing = max(params.spacing, 0.05);
    let gap_y = spacing * bpb * step;
    let travel = (params.beats / bpb + params.drift) / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    // Every second line half a gap later.
    let shift = 0.5 * f32(row % 2u);
    let first = ceil(params.near / gap_y + part - shift);
    let k = first + f32(cell.y);
    let y = gap_y * (k - part + shift);
    if y > params.far {
        return none(0.0, y);
    }
    // The pillar's own draws, the same for both its parts and while it comes nearer.
    let n = i32(whole + k);
    let seed = u32(max(params.seed, 0));
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(n), column, 53u + seed * 7u))) / 4294967296.0;
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, side == 1u);
    let off = params.lane + 0.5 * params.across * (1.0 + abs(lane.g)) + f32(row) * params.gap;
    let x = lane.r + sign * off;
    if wired && !on_floor(y, step) {
        return none(x, y);
    }
    // The gap: its middle and height from the pillar's draws, a little taller on the start of
    // each beat, rising quickly and settling over the beat.
    let ceiling = max(params.ceiling, 1.0);
    let middle = mix(params.low, params.high, r.x);
    let b = clamp(params.beat, 0.0, 1.0);
    let kick = smoothstep(0.0, 0.08, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
    let open = mix(params.narrow, params.wide, r.z) * (1.0 + params.pulse * kick);
    // Presence: each pillar has its own share of it, so as presence falls one pillar after
    // another sinks back; at a steady presence a pillar stands whole or not at all.
    let own = clamp((params.presence * 2.0 - r.y) * 8.0, 0.0, 1.0);
    let size = own * own * (3.0 - 2.0 * own);
    var tall = max(middle - 0.5 * open - LIFT, 0.0);
    if top {
        tall = max(ceiling - LIFT - middle - 0.5 * open, 0.0);
    }
    tall *= size;
    if tall < SHORTEST {
        return none(x, y);
    }
    var z = LIFT + 0.5 * tall;
    if top {
        z = ceiling - LIFT - 0.5 * tall;
    }
    return vec4f(x, y, z, tall);
}
