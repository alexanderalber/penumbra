// The glider's course, read from the stage's lane picture (lane.wgsl) in in0. Render it 1 x 1
// and read it with chan.from_tex: red the lane's x where the glider flies (`at` metres ahead),
// green the turn about z (nose along the lane, degrees), blue the roll into the turn (degrees).
// @param at float = 3 -8..56 -- where the glider flies, metres ahead of the camera
// @param bank float = 1.5 0..5 -- how far the glider leans into a turn

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane = textureSampleLevel(in0, pn_clamp, vec2f((params.at - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.5), 0.0);
    let heading = degrees(atan(lane.g));
    // x, the turn about z (nose towards the lane), the roll into the turn
    return vec4f(lane.r, -heading, params.bank * heading, 1.0);
}
