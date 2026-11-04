// The glider's course, read from the stage's lane picture (the stage's `lane` output) in in0.
// Render it 1 x 1 and read it with chan.from_tex: red the glider's x across, green the turn
// about z (nose along its course, degrees), blue the roll (degrees), alpha the height of the
// traps' corridor (metres above the floor, for flight.wgsl).
// The glider flies `at` metres ahead; towards `presence` 0 it flies further ahead, as
// flight.wgsl places it, and follows the lane there.
// Three shares mix its flight; the component sets them from its `flight` row:
// - `follow` 1 flies along the lane's turns and leans into them; 0 flies straight down the
//   lane's middle (the lane's mean over 16 m), nose ahead and level.
// - `roll` 1 rolls the glider once round its length on a downbeat every `roll_bars` bars,
//   over `roll_beats` beats, the other way round each time. It rolls only while `roll` is above
//   one half.
// - `lean` 1 leans the glider to one side on each downbeat and back over the bar, the other
//   side on the next bar, and moves it a little that way.
// - `thread` 1 flies along the free corridor through mid_traps' beams, 2 m further ahead: across
//   the lane's middle by the corridor's x, nose along the corridor, leaning into its turns. The corridor's params
//   (`seed` and the rest) come from the traps' `corridor` output; flight.wgsl takes the height.
// @param at float = 3 -8..56 -- where the glider flies, metres ahead of the camera
// @param presence float = 1 0..1 -- 1 the glider flies at `at`, towards 0 it climbs away ahead
// @param bank float = 1.5 0..5 -- how far the glider leans into a turn
// @param follow float = 1 0..1 -- 1 along the lane's turns, 0 straight down its middle
// @param roll float = 0 0..1 -- above one half the glider rolls on a downbeat every `roll_bars` bars
// @param lean float = 0 0..1 -- how much the glider leans on each downbeat
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param roll_bars float = 4 1..16 -- bars from one roll to the next
// @param roll_beats float = 2 0.5..8 -- beats a roll takes
// @param lean_angle float = 22 0..60 -- degrees the glider leans on a downbeat at `lean` 1
// @param thread float = 0 0..1 -- how much the glider follows the traps' corridor
// @param seed float = 1 0..1000 -- the traps' seed (from their `corridor` output)
// @param corridor_bars float = 2 0.5..8 -- bars from one knot of the corridor to the next (from the traps)
// @param wander float = 1.2 0..2.5 -- metres the corridor moves to either side (from the traps)
// @param climb float = 0.5 0..1.5 -- metres the corridor moves up and down (from the traps)
// @param middle float = 2 0.5..4 -- the corridor's middle height, metres above the floor (from the traps)
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres the glider flies ahead of `at` at presence 0 (flight.wgsl's AWAY).
const AWAY: f32 = 6.0;
// Metres the glider flies further ahead along the traps' corridor (flight.wgsl's THREAD_AHEAD):
// seen from further back, its swerves read as moves across the tube.
const THREAD_AHEAD: f32 = 2.0;
// The stretch of lane whose mean is its middle: two of its zigzags at the stage's defaults.
const SPAN: f32 = 16.0;
const SAMPLES: i32 = 33;
// Metres the glider moves sideways with a full lean.
const SHIFT: f32 = 0.3;
// Beats the lean takes to reach its side after the downbeat.
const LEAN_IN: f32 = 0.35;
// Bars on either side of the glider over which the corridor's slope is taken.
const SLOPE_BARS: f32 = 0.05;
// The share of the corridor's turn the glider's nose follows: it slips sideways for the rest,
// so a quick dodge does not turn it broadside to the camera.
const THREAD_TURN: f32 = 0.5;

// corridor begin: library/mid_traps/traps.wgsl holds the same code from here to "corridor end".
// The free corridor of mid_traps: where its middle lies across (x, metres from the middle) and
// up (z, metres above the floor) at `bars` bars of travel, a whole number on a downbeat. The
// corridor has a knot every `every` bars: left of the middle at one knot, right at the next, and
// up or down by up to `climb`. It holds at a knot for an eighth of the way to the next one and
// then glides there. `seed` picks the knots.
fn corridor_knot(n: f32, seed: f32, wander: f32, climb: f32, middle: f32) -> vec2f {
    let k = i32(n);
    let h = pn_hash(vec3u(bitcast<u32>(k), bitcast<u32>(i32(seed)), 0x7a95u));
    let across = 0.35 + 0.65 * f32(h.x) / 4294967296.0;
    let up = 2.0 * f32(h.y) / 4294967296.0 - 1.0;
    let side = select(-1.0, 1.0, (k & 1) == 0);
    return vec2f(side * max(wander, 0.0) * across, middle + max(climb, 0.0) * up);
}

fn corridor(bars: f32, every: f32, seed: f32, wander: f32, climb: f32, middle: f32) -> vec2f {
    let t = bars / max(every, 0.5);
    let n = floor(t);
    let f = clamp((t - n - 0.125) / 0.875, 0.0, 1.0);
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = corridor_knot(n, seed, wander, climb, middle);
    let b = corridor_knot(n + 1.0, seed, wander, climb, middle);
    return mix(a, b, s);
}
// corridor end

fn corridor_now(bars: f32) -> vec2f {
    return corridor(bars, params.corridor_bars, params.seed, params.wander, params.climb, params.middle);
}

fn lane_at(y: f32) -> vec4f {
    return textureSampleLevel(in0, pn_clamp, vec2f((y - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.5), 0.0);
}

// The lane's middle across near y: its mean over SPAN metres around y.
fn middle(y: f32) -> f32 {
    var sum = 0.0;
    for (var k = 0; k < SAMPLES; k = k + 1) {
        let t = f32(k) / f32(SAMPLES - 1) - 0.5;
        sum = sum + lane_at(y + t * SPAN).r;
    }
    return sum / f32(SAMPLES);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let a = 1.0 - clamp(params.presence, 0.0, 1.0);
    let thread = clamp(params.thread, 0.0, 1.0);
    let y = params.at + AWAY * a * a * (3.0 - 2.0 * a) + THREAD_AHEAD * thread;
    let lane = lane_at(y);
    let follow = clamp(params.follow, 0.0, 1.0);
    let heading = degrees(atan(lane.g)) * follow;

    // Bars counted so that a whole number is a downbeat.
    let bpb = max(params.beats_per_bar, 1.0);
    let bars = params.beats / bpb + params.drift;
    let bar = floor(bars);
    let into = (bars - bar) * bpb;

    // The lean: in quickly after the downbeat, back over the rest of the bar.
    let side = select(-1.0, 1.0, (i32(bar) & 1) == 0);
    let shape = smoothstep(0.0, LEAN_IN, into) * (1.0 - smoothstep(LEAN_IN, bpb, into));
    let lean = clamp(params.lean, 0.0, 1.0) * side * shape;

    // The roll: one turn round the glider's length, eased in and out.
    let every = max(round(params.roll_bars), 1.0);
    let lap = floor(bars / every);
    let phase = (bars - lap * every) * bpb / max(params.roll_beats, 0.1);
    let turning = select(0.0, 1.0, params.roll > 0.5 && phase < 1.0);
    let p = clamp(phase, 0.0, 1.0);
    let eased = p * p * p * (p * (p * 6.0 - 15.0) + 10.0);
    let way = select(-1.0, 1.0, (i32(lap) & 1) == 0);
    let rolled = 360.0 * eased * way * turning;

    // The traps' corridor where the glider flies, and its slope across per metre ahead.
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let step = max(select(params.step, lane0.b, lane0.b > 0.0), 0.01);
    let here = (params.beats + y / step) / bpb + params.drift;
    let free = corridor_now(here);
    let ahead = corridor_now(here + SLOPE_BARS);
    let behind = corridor_now(here - SLOPE_BARS);
    let slope = (ahead.x - behind.x) / (2.0 * SLOPE_BARS * bpb * step);
    let along = mix(heading, degrees(atan(slope)) * THREAD_TURN, thread);

    let centre = middle(y);
    let x = mix(mix(centre, lane.r, follow) + SHIFT * lean, centre + free.x, thread);
    let roll = params.bank * along + params.lean_angle * lean + rolled;
    // x, the turn about z (nose along the course), the roll, the corridor's height
    return vec4f(x, -along, roll, free.y);
}
