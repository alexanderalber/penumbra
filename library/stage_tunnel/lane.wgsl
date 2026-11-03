// The lane of the tunnel stage. The lane zigzags as on the synthwave stage, but the tube rolls
// with it instead: a lane that swings x metres to the side turns the tube by x over its radius
// (times the stage's `twist`), so the lane always lies at the tube's bottom, straight ahead.
//
// `what` twist: the zigzag as the synthwave stage's lane picture has it, for the tube's roll.
// Render it 256 x 1. Texel i covers y = -8 + i * 0.25 to y = -8 + (i + 1) * 0.25 metres ahead of
// the camera, so u = (y + 8) / 64 samples it at y with linear filtering:
//   red    how far the lane swings across (x, metres)
//   green  how far it moves sideways per metre ahead (dx / dy)
//   blue   `step`, metres the world moves per beat, the same in every texel
//   alpha  the tube there: 1 where it stands whole, 0 in the open middle of a gap, between where
//          it runs out at a gap's ends or while a gap opens
// `what` lane: the lane picture for the mid elements, in the same layout. The lane lies straight
// at the tube's bottom, so red and green are 0. Blue is `step`. Alpha is 0: the tube's bottom is
// curved, so no floor element can stand on it, and floor elements sink away where alpha is 0.
// in0: the gaps' memory (gaps.wgsl), optional: with it a change of `gap` reaches only the tube
// beyond the fade's end and comes towards the camera with the tube; without it the gaps follow
// `gap` at once.
// @param what choice(twist|lane) = twist -- which picture
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param step float = 0.5 0.1..2 -- metres the world moves per beat; the distance between two rings
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param bars_per_turn float = 2 1..8 -- bars between two turns of the lane
// @param turn float = 4 0.1..8 -- beats a turn takes
// @param swing float = 1.5 0..5 -- how far the lane swings to either side at most, in metres
// @param origin float = 11 1..50 -- metres ahead where the lane's turns are counted: a corner passes there on a downbeat
// @param gap float = 0 0..1 -- share of the tube that runs out into gaps: 0 none, 1 all of it (read while in0 is not wired)
// @param gap_length float = 8 1..40 -- metres a gap runs along the tube, rounded to whole bars

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

// The lane's place in turns at distance c: the grid's bar count there, so a turn starts on a
// downbeat. The zigzag repeats every two turns, so only `drift` within two turns matters; taking
// it modulo keeps the sum small and exact in f32.
fn turns(c: f32) -> f32 {
    let lap = 2.0 * params.bars_per_turn;
    let phase = params.drift - lap * floor(params.drift / lap);
    return (c / params.beats_per_bar + phase) / params.bars_per_turn;
}

// Width of a corner, in turns.
fn corner() -> f32 {
    return clamp(params.turn / (params.beats_per_bar * params.bars_per_turn), 0.001, 1.0);
}

// The zigzag through corners at +1 and -1 in turn, each corner rounded over `corner()` just
// after its knot. The heading eases from the old leg's to the new one's (smoothstep), so a turn
// starts on the downbeat and ends exactly `turn` beats later, with no jolt at either end. This
// is the zigzag smoothed with the kernel 6 x (1 - x) over that width, in closed form. It repeats
// every two turns.
fn zigzag(u: f32) -> f32 {
    let w = corner();
    let i = floor(u);
    let f = u - i;
    let p = select(-1.0, 1.0, (i32(i) & 1) == 0);
    let now = -2.0 * p;
    let before = 2.0 * p;
    let straight = p + now * (f - 0.5 * w);
    let a = min(f / w, 1.0);
    let rest = 0.5 - a + a * a * a - 0.5 * a * a * a * a;
    return straight - (before - now) * w * rest;
}

// The zigzag's slope, per turn.
fn zigzag_slope(u: f32) -> f32 {
    let i = floor(u);
    let p = select(-1.0, 1.0, (i32(i) & 1) == 0);
    return mix(2.0 * p, -2.0 * p, smoothstep(0.0, 1.0, (u - i) / corner()));
}

// How far the lane swings, changing slowly with the distance (over 32 beats): some turns wide,
// some narrow.
fn swing_at(i: f32) -> f32 {
    let h = f32(pn_hash(vec3u(bitcast<u32>(i32(i)), 11u, 0u)).x) / 4294967296.0;
    return mix(0.3, 1.0, h) * params.swing;
}

fn swing(c: f32) -> vec2f {
    let s = c / 32.0;
    let i = floor(s);
    let f = s - i;
    let a = swing_at(i);
    let b = swing_at(i + 1.0);
    let e = smoothstep(0.0, 1.0, f);
    // the swing and its change per beat
    return vec2f(mix(a, b, e), (b - a) * 6.0 * f * (1.0 - f) / 32.0);
}

// The lane at distance c: c is the distance travelled when a point of the tube passed `origin`.
fn lane_x(c: f32) -> f32 {
    return zigzag(turns(c)) * swing(c).x;
}

// The lane's change per beat at distance c.
fn lane_slope(c: f32) -> f32 {
    let u = turns(c);
    let sw = swing(c);
    return zigzag_slope(u) / (params.beats_per_bar * params.bars_per_turn) * sw.x + zigzag(u) * sw.y;
}

// Share of a gap's growth over which it opens: a gap fades in as `gap` rises past its draw.
const GAP_SOFT: f32 = 0.08;
// How far the tube runs out at each end of a gap: this share of the gap's length, at most
// `RUN_OUT_MOST` metres. The middle of a gap is open sky.
const RUN_OUT_SHARE: f32 = 0.4;
const RUN_OUT_MOST: f32 = 4.0;

// Cells of the gaps' memory (gaps.wgsl) in in0.
const GAP_CELLS: f32 = 512.0;

// The share of the tube in gaps that stretch `i` (of `bars` bars) keeps: what the gaps' memory in
// in0 held at its first cell when that cell was last out of sight, so a change of `gap` comes in
// from the far end. Without the memory, `gap` itself.
fn gap_of(i: f32, bars: f32, bpb: f32) -> f32 {
    if textureDimensions(in0).x < u32(GAP_CELLS) {
        return params.gap;
    }
    let first = floor((i * bars - params.drift) * bpb);
    let k = i32(first - GAP_CELLS * floor(first / GAP_CELLS));
    return textureLoad(in0, vec2i(k, 0), 0).r;
}

// How far stretch `i` is open: 0 the tube, 1 a gap. A stretch is a gap when its draw lies under
// its share of gaps (gap_of).
fn opened(i: f32, bars: f32, bpb: f32) -> f32 {
    let gap = gap_of(i, bars, bpb);
    if gap <= 0.0 {
        return 0.0;
    }
    let draw = f32(pn_hash(vec3u(bitcast<u32>(i32(i)), 23u, 5u)).x) / 4294967296.0;
    return smoothstep(0.0, GAP_SOFT, gap * (1.0 + GAP_SOFT) - draw);
}

// The tube at the point that passes the camera when `travelled` is `c` beats: 1 where it stands
// whole, 0 in the open middle of a gap, between where it runs out. The tube is cut into
// stretches of whole bars, each about `gap_length` long, so a gap starts on a bar. Over the
// first and last metres of a gap the tube fades: the stage thins out its rings and fades its
// lines and wall there. Where two gaps follow each other the tube does not come back between
// them.
fn ground(c: f32) -> f32 {
    let bpb = max(params.beats_per_bar, 1.0);
    let step = max(params.step, 0.01);
    let bars = max(round(params.gap_length / (bpb * step)), 1.0);
    let at = (c / bpb + params.drift) / bars;
    let stretch = floor(at);
    let f = at - stretch;
    let open = opened(stretch, bars, bpb);
    if open <= 0.0 {
        return 1.0;
    }
    let span = bars * bpb * step;
    let ramp = max(min(RUN_OUT_SHARE * span, RUN_OUT_MOST), 0.01);
    let from_start = mix(smoothstep(0.0, ramp, f * span), 1.0, opened(stretch - 1.0, bars, bpb));
    let to_end = mix(smoothstep(0.0, ramp, (1.0 - f) * span), 1.0, opened(stretch + 1.0, bars, bpb));
    return 1.0 - open * min(from_start, to_end);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let y = mix(LANE_NEAR, LANE_FAR, v.uv.x);
    // The point at y passed `origin` this many beats ago.
    let born = params.beats - (params.origin - y) / params.step;
    if params.what == what_lane {
        return vec4f(0.0, 0.0, params.step, 0.0);
    }
    let here = ground(params.beats + y / params.step);
    return vec4f(lane_x(born), lane_slope(born) / params.step, params.step, here);
}
