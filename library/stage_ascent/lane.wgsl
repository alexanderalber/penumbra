// The lane of the ascent stage. The lane zigzags as on the synthwave stage, but the camera
// climbs instead of flying along it: the lane's swing at the camera sways the tower around its
// axis instead (times the stage's `twist`, over its radius), so the camera seems to circle.
//
// `what` twist: the zigzag as the synthwave stage's lane picture has it, for the tower's sway.
// Render it 256 x 1. Texel i covers y = -8 + i * 0.25 to y = -8 + (i + 1) * 0.25 metres ahead of
// the camera, so u = (y + 8) / 64 samples it at y with linear filtering:
//   red    how far the lane swings across (x, metres)
//   green  how far it moves sideways per metre ahead (dx / dy)
//   blue   `step`, metres the world moves per beat, the same in every texel
//   alpha  1
// `what` lane: the lane picture for the mid elements, in the same layout. The lane runs straight
// ahead through the shaft, so red and green are 0. Blue is `step`. Alpha is 0: there is no floor
// in the shaft, so no floor element can stand there, and floor elements sink away where alpha is 0.
// @param what choice(twist|lane) = twist -- which picture
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param step float = 0.5 0.1..2 -- metres the world moves per beat, for the mid elements
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param bars_per_turn float = 2 1..8 -- bars between two turns of the lane
// @param turn float = 4 0.1..8 -- beats a turn takes
// @param swing float = 1.5 0..5 -- how far the lane swings to either side at most, in metres
// @param origin float = 11 1..50 -- metres ahead where the lane's turns are counted: a corner passes there on a downbeat

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

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let y = mix(LANE_NEAR, LANE_FAR, v.uv.x);
    // The point at y passed `origin` this many beats ago.
    let born = params.beats - (params.origin - y) / params.step;
    if params.what == what_lane {
        return vec4f(0.0, 0.0, params.step, 0.0);
    }
    return vec4f(lane_x(born), lane_slope(born) / params.step, params.step, 1.0);
}
