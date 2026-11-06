// Where the galloping horse of floor_animal runs, which way it faces and which of its 24 poses
// shows. Render it 1 x 1 for each geo.tex_instance: one node writes the place, one the aim, and
// one per pose the scale, which is 0 for every pose but the one of the moment.
// The horse runs beside the lane, `at` metres ahead on average. It runs a little faster than the
// flight for half of a long cycle and a little slower for the other half, so it overtakes the
// camera's view and falls back by `range` metres each way. The cycle takes `cycle_min` to
// `cycle_max` bars, set by `seed`. Its legs follow its way over the floor: a stride per
// `STRIDE` metres the horse moves on the floor (animal.py's STRIDE, along its turned body), so the
// hooves on the ground stand still on it. At the stage's 0.5 m per beat that is a stride per 2
// beats, a little faster while it gains on the camera, slower while it falls back.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0
// and no gaps.
// `what` place: where the model's origin goes (the ground under the middle of its body). aim:
// where its nose points: along the lane, turned in by `turn`. scale: its size, 0 unless `pose` is the pose of the moment.
// @param what choice(place|aim|scale) = place -- what this node writes
// @param pose int = 0 0..23 -- the pose this node's scale is for
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param presence float = 1 0..1 -- 1 the horse runs beside the camera, towards 0 it runs off ahead and shrinks, 0 none
// @param seed int = 1 0..1000 -- another number gives another length of the cycle and another start in it
// @param at float = 5 2..8 -- metres ahead of the camera the horse runs on average
// @param range float = 1.5 0..4 -- metres it gains on the camera and falls back
// @param cycle_min float = 16 2..128 -- the shortest cycle of gaining and falling back, in bars
// @param cycle_max float = 32 2..128 -- the longest one
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param side float = 1 -1..1 -- which side of the lane it runs on: 1 right, -1 left
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 1.3 0..5 -- metres between the lane's edge and the middle of the horse
// @param turn float = 30 -60..60 -- degrees the horse faces in towards the lane, so the camera sees more of its side
// @param size float = 1 0.3..2 -- size of the horse: 1 is 1.6 m to the withers (the camera is 1.5 to 2.9 m up)

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const POSES: f32 = 24.0;
// Metres the horse moves forwards in one stride (animal.py's STRIDE).
const STRIDE: f32 = 1.155;
// Metres the horse runs ahead at presence 0, out through the stage's distance fade.
const AWAY: f32 = 8.0;
const TAU: f32 = 6.2831853;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let p = clamp(params.presence, 0.0, 1.0);
    let h = vec2f(pn_hash(vec3u(u32(max(params.seed, 0)), 31u, 7u)).xy) / 4294967296.0;
    let lo = max(min(params.cycle_min, params.cycle_max), 1.0);
    let hi = max(params.cycle_max, lo);
    let cycle = floor(mix(lo, hi + 0.999, h.x));
    // Bars travelled, split into whole cycles and the part of one, for precision late in a file.
    let bars = params.beats / max(params.beats_per_bar, 1.0);
    let turn = fract(bars / cycle - floor(bars / cycle) + h.y);
    let away = 1.0 - p;
    let y = params.at + params.range * sin(TAU * turn) + AWAY * away * away * away;
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, params.side >= 0.0);
    let size = params.size;
    let x = lane.r + sign * (params.lane * (1.0 + 0.5 * abs(lane.g)) + params.clear * size);
    if params.what == what_place {
        return vec4f(x, y, 0.0, 1.0);
    }
    if params.what == what_aim {
        // Turned towards the lane by `turn`, so the camera sees its flank rather than its back.
        let d = normalize(vec2f(lane.g, 1.0));
        let a = sign * radians(params.turn);
        return vec4f(cos(a) * d.x - sin(a) * d.y, sin(a) * d.x + cos(a) * d.y, 0.0, 1.0);
    }
    // The stride follows the horse's way over the floor: the camera's travel plus its own place
    // ahead. Its body is turned by `turn`, so it moves forwards by STRIDE x cos(turn) along the
    // lane per stride. The whole beats are split off first, for precision late in a file.
    let step = select(0.5, lane0.b, wired);
    let along = STRIDE * max(cos(radians(params.turn)), 0.2);
    let whole = floor(params.beats);
    let ground = fract(whole * step / along) + ((params.beats - whole) * step + y) / along;
    let phase = fract(ground);
    let now = min(floor(phase * POSES), POSES - 1.0);
    // A horse over a gap in the floor goes with it; towards presence 0 it shrinks away.
    var s = size * smoothstep(0.0, 0.35, p) * clamp(lane.a, 0.0, 1.0);
    if abs(now - f32(params.pose)) > 0.5 || p < 0.01 || s < 0.02 {
        s = 0.0;
    }
    return vec4f(vec3f(s), 1.0);
}
