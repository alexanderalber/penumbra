// The city: one block per texel, drawn by geo.tex_instance. A column of texels is a street
// across (x), a row a place along it (y). Every block comes towards the camera at `speed` and,
// once past it, starts again far ahead with a new size and place. Low blocks keep a lane free
// for the glider. The lane drifts slowly from side to side: each block takes the lane's place
// at the moment it appears, so the lane is a winding path that comes towards the camera, and
// the glider, `at` metres ahead, steers along it as it passes.
// @param output choice(position|scale|steer) = position -- what this node hands on: where each block stands, its size, or the glider's course (render it at 1 x 1)
// @param speed float = 1.0417 0..4 -- metres per second towards the camera; the grid moves as fast
// @param near float = -1 -10..10 -- where the blocks leave, behind the camera
// @param far float = 11 1..50 -- where they appear, growing out of the floor
// @param spread float = 12 1..40 -- width of the city across
// @param lane float = 1.6 0..5 -- half the width of the low lane
// @param drift float = 1.5 0..5 -- how far the lane wanders to either side, in metres
// @param drift_rate float = 0.1 0..1 -- how often the lane picks a new direction, per second
// @param at float = 3.2 0..20 -- steer: where the glider flies, metres ahead of the camera
// @param bank float = 1.5 0..5 -- steer: how far the glider leans into a turn
// @param fill float = 0.7 0..1 -- share of places that hold a block

fn knot(i: f32) -> f32 {
    let h = pn_hash(vec3u(bitcast<u32>(i32(i)), 11u, 0u)).x;
    return f32(h) / 2147483648.0 - 1.0;
}

// Where the lane runs at time t: smooth noise through a random place every 1 / drift_rate
// seconds (Catmull-Rom, so the course has no kinks).
fn lane_x(t: f32) -> f32 {
    let s = t * params.drift_rate;
    let i = floor(s);
    let f = s - i;
    let p0 = knot(i - 1.0);
    let p1 = knot(i);
    let p2 = knot(i + 1.0);
    let p3 = knot(i + 2.0);
    let a = 2.0 * p1 + f * (p2 - p0) + f * f * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3)
        + f * f * f * (3.0 * p1 - p0 - 3.0 * p2 + p3);
    return 0.5 * a * params.drift;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    if params.output == output_steer {
        // The blocks at `at` appeared at `far` this long ago, and took the lane of then.
        let t = pn_time.seconds - (params.far - params.at) / params.speed;
        let x = lane_x(t);
        let h = 0.05;
        let sideways = (lane_x(t + h) - lane_x(t - h)) / (2.0 * h);
        let heading = degrees(atan2(sideways, params.speed));
        // x, the turn about z (nose towards the lane), the roll into the turn
        return vec4f(x, -heading, params.bank * heading, 1.0);
    }
    let cell = vec2u(v.pos.xy);
    let size = pn.resolution;
    let span = params.far - params.near;
    let spacing = span / size.y;
    // Each street runs a little out of step with its neighbours.
    let street = f32(pn_hash(vec3u(cell.x, 7u, 0u)).x) / 4294967296.0;
    let ahead = (f32(cell.y) + street) * spacing;
    let travelled = pn_time.seconds * params.speed + ahead;
    let lap = floor(travelled / span);
    let y = params.far - (travelled - lap * span);
    // A new block on every lap: three random numbers for its side, place and height.
    let r = vec3f(pn_hash(vec3u(cell, u32(lap)))) / 4294967296.0;
    let slot = params.spread / size.x;
    let side = min(mix(0.25, 0.5, r.x), 0.9 * min(slot, spacing));
    let x = -0.5 * params.spread + (f32(cell.x) + 0.5) * slot + (r.y - 0.5) * (slot - side);
    // The lane as it was when this block appeared at `far`.
    let born = (lap * span - ahead) / params.speed;
    var height = select(mix(0.2, 1.4, r.z), mix(0.04, 0.24, r.z), abs(x - lane_x(born)) < params.lane);
    // Grow out of the floor over the first two metres; leave out the empty places.
    height *= smoothstep(params.far, params.far - 2.0, y);
    let present = fract(r.x * 97.0 + r.z * 13.0) < params.fill;
    if !present {
        height = 0.0;
    }
    if params.output == output_scale {
        return vec4f(select(0.0, side, present), select(0.0, side, present), height, 1.0);
    }
    return vec4f(x, y, 0.5 * height, 1.0);
}
