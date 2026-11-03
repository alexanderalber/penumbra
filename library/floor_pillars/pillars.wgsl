// Pillars in lines beside the lane of the synthwave stage, one per texel, drawn by
// geo.tex_instance with a prism. A column of texels is a line of pillars (left lines first, then
// right ones, `rows` per side, at most 3), a row a pillar along it. The pillars stand
// `spacing` bars of travel apart, so they pass in rhythm: one passes the camera on a downbeat.
// They follow the lane at a fixed distance from its edge. A pillar keeps its place on the floor
// while it comes nearer, since the lane is fixed to the floor.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// Out: x, y, z of the pillar's centre in rgb, its width in alpha (0 where no pillar stands). A
// second node (sizes.wgsl) turns that into the pillars' scale. Render it 6 x 32.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the pillars stretch a little on its start
// @param presence float = 1 0..1 -- how many pillars stand: 1 all, towards 0 they sink into the floor one after another, 0 none
// @param spacing float = 1 0.25..8 -- bars of travel between two pillars of a line
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param tall float = 2.4 0.2..6 -- height of a pillar, in metres (the camera is 2 m up, the ceiling 4 m)
// @param across float = 0.3 0.05..2 -- width of a pillar, in metres
// @param rows int = 1 1..3 -- lines of pillars on each side of the lane
// @param gap float = 1.2 0.3..5 -- metres between two lines on one side
// @param pulse float = 0.1 0..0.5 -- how far a pillar stretches on the beat, as a share of its height
// @param lane float = 1.6 0..5 -- half the width of the free lane; the nearest line stands just outside it
// @param near float = -2 -8..10 -- where the pillars leave, behind the camera
// @param far float = 14 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres a pillar's base stands above the floor.
const LIFT: f32 = 0.02;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let rows = u32(clamp(params.rows, 1, 3));
    let side = cell.x / 3u;
    let row = cell.x % 3u;
    if row >= rows || side > 1u {
        return vec4f(0.0);
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
    let first = ceil(params.near / gap_y + part);
    let k = first + f32(cell.y);
    let y = gap_y * (k - part);
    if y > params.far {
        return vec4f(0.0);
    }
    // The pillar's own number, the same while it comes nearer.
    let n = i32(whole + k);
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(n), cell.x, 31u))) / 4294967296.0;
    // The lane where the pillar stands; a slanting lane widens the margin.
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, side == 1u);
    let off = params.lane + 0.5 * params.across * (1.0 + abs(lane.g)) + f32(row) * params.gap;
    let x = lane.r + sign * off;
    var height = params.tall * mix(0.85, 1.1, r.x);
    // A little taller on the start of each beat, rising quickly and settling over the beat.
    let b = clamp(params.beat, 0.0, 1.0);
    let kick = smoothstep(0.0, 0.08, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
    height *= 1.0 + params.pulse * kick;
    // Presence: each pillar has its own share of it, so as presence falls the pillars sink into
    // the floor one after another. At 0.5 about half the height is left.
    let own = clamp(params.presence * 2.0 - r.y, 0.0, 1.0);
    height *= own * own * (3.0 - 2.0 * own);
    // Where the floor has a gap, the pillars go with it.
    height *= clamp(lane.a, 0.0, 1.0);
    // A pillar under 3 cm is left out, and every pillar stands `LIFT` above the floor, so its
    // base does not flicker against the floor's lines.
    if height < 0.03 {
        return vec4f(x, y, 0.0, 0.0);
    }
    return vec4f(x, y, 0.5 * height + LIFT, params.across);
}
