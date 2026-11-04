// The glider's course, read from the stage's lane picture (the stage's `lane` output) in in0.
// Render it 1 x 1 and read it with chan.from_tex: red the glider's x across, green the turn
// about z (nose along its course, degrees), blue the roll (degrees).
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

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres the glider flies ahead of `at` at presence 0 (flight.wgsl's AWAY).
const AWAY: f32 = 6.0;
// The stretch of lane whose mean is its middle: two of its zigzags at the stage's defaults.
const SPAN: f32 = 16.0;
const SAMPLES: i32 = 33;
// Metres the glider moves sideways with a full lean.
const SHIFT: f32 = 0.3;
// Beats the lean takes to reach its side after the downbeat.
const LEAN_IN: f32 = 0.35;

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
    let y = params.at + AWAY * a * a * (3.0 - 2.0 * a);
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

    let x = mix(middle(y), lane.r, follow) + SHIFT * lean;
    let roll = params.bank * heading + params.lean_angle * lean + rolled;
    // x, the turn about z (nose along the course), the roll
    return vec4f(x, -heading, roll, 1.0);
}
