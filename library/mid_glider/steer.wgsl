// The glider's course, read from the stage's lane picture (the stage's `lane` output) in in0.
// Render it 1 x 1 and read it with chan.from_tex: red the lane's x where the glider flies,
// green the turn about z (nose along the lane, degrees), blue the roll into the turn (degrees).
// The glider flies `at` metres ahead; towards `presence` 0 it flies further ahead, as
// flight.wgsl places it, and follows the lane there.
// @param at float = 3 -8..56 -- where the glider flies, metres ahead of the camera
// @param presence float = 1 0..1 -- 1 the glider flies at `at`, towards 0 it climbs away ahead
// @param bank float = 1.5 0..5 -- how far the glider leans into a turn

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres the glider flies ahead of `at` at presence 0 (flight.wgsl's AWAY).
const AWAY: f32 = 6.0;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let a = 1.0 - clamp(params.presence, 0.0, 1.0);
    let y = params.at + AWAY * a * a * (3.0 - 2.0 * a);
    let lane = textureSampleLevel(in0, pn_clamp, vec2f((y - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.5), 0.0);
    let heading = degrees(atan(lane.g));
    // x, the turn about z (nose towards the lane), the roll into the turn
    return vec4f(lane.r, -heading, params.bank * heading, 1.0);
}
