// The lane of the synthwave stage, as it lies on the floor now: a zigzag the glider flies along
// and that floor elements keep free. It runs straight and swings to the other side every
// `bars_per_turn` bars, so a turn starts on a downbeat. `beats` is the distance travelled in
// beats; the floor moves `step` metres per beat.
//
// The picture is the lane's contract with the floor elements. Render it 256 x 1. Texel i covers
// the floor from y = -8 + i * 0.25 to y = -8 + (i + 1) * 0.25 metres ahead of the camera, so
// u = (y + 8) / 64 samples it at y with linear filtering:
//   red    the lane's middle across (x, metres)
//   green  how far it moves sideways per metre ahead (dx / dy)
//   blue   `step`, metres the floor moves per beat, the same in every texel
//   alpha  the floor there: 1 where it stands, 0 in a gap, between while a gap opens or closes
// The lane is fixed to the floor: a point of the floor keeps its lane value as it comes nearer.
// So are the gaps: a gap begins where a bar begins, so its near edge passes the camera on a
// downbeat. Floor elements read alpha to sink where the floor has gone.
// in0: the gaps' memory (gaps.wgsl), optional: with it a change of `gap` reaches only the floor
// beyond the fade's end and comes towards the camera with the floor; without it the gaps follow
// `gap` at once.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param step float = 0.5 0.1..2 -- metres the floor moves per beat; one grid cell
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param bars_per_turn float = 2 1..8 -- bars between two turns of the lane
// @param turn float = 4 0.1..8 -- beats a turn takes
// @param swing float = 1.5 0..5 -- how far the lane swings to either side at most, in metres
// @param origin float = 11 1..50 -- metres ahead where the lane's turns are counted: a corner passes there on a downbeat
// @param gap float = 0 0..1 -- share of the floor that breaks off into gaps: 0 none, 1 all of it (read while in0 is not wired)
// @param gap_length float = 8 1..40 -- metres a gap runs along the floor, rounded to whole bars

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

// The lane at distance c: c is the distance travelled when a point of the floor passed `origin`.
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
// Cells of the gaps' memory (gaps.wgsl) in in0.
const GAP_CELLS: f32 = 512.0;

// The share of the floor in gaps that stretch `stretch` (of `bars` bars) keeps: what the gaps'
// memory in in0 held at its first cell when that cell was last out of sight, so a change of `gap`
// comes in from the horizon. Without the memory, `gap` itself.
fn gap_of(stretch: f32, bars: f32, bpb: f32) -> f32 {
    if textureDimensions(in0).x < u32(GAP_CELLS) {
        return params.gap;
    }
    let first = ceil((stretch * bars - params.drift) * bpb - 0.5);
    let i = i32(first - GAP_CELLS * floor(first / GAP_CELLS));
    return textureLoad(in0, vec2i(i, 0), 0).r;
}

// The floor at the point that passes the camera when `travelled` is `c` beats: 1 where it stands,
// 0 in a gap. The floor is cut into stretches of whole bars, each about `gap_length` long; a
// stretch is a gap when its draw lies under its share of gaps (gap_of). The bar is taken at the
// middle of the point's beat, so a grid cell is wholly in or out.
fn ground(c: f32) -> f32 {
    let bpb = max(params.beats_per_bar, 1.0);
    let bars = max(round(params.gap_length / max(bpb * params.step, 0.01)), 1.0);
    let bar = floor((floor(c) + 0.5) / bpb + params.drift);
    let stretch = floor(bar / bars);
    let gap = gap_of(stretch, bars, bpb);
    if gap <= 0.0 {
        return 1.0;
    }
    let draw = f32(pn_hash(vec3u(bitcast<u32>(i32(stretch)), 23u, 5u)).x) / 4294967296.0;
    let open = smoothstep(0.0, GAP_SOFT, gap * (1.0 + GAP_SOFT) - draw);
    return 1.0 - open;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let y = mix(LANE_NEAR, LANE_FAR, v.uv.x);
    // The point at y passed `origin` this many beats ago.
    let born = params.beats - (params.origin - y) / params.step;
    let floor_here = ground(params.beats + y / params.step);
    return vec4f(lane_x(born), lane_slope(born) / params.step, params.step, floor_here);
}
