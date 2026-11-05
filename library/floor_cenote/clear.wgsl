// The lane of the valley stage with floor_cenote's clearings cut into its shore. It passes the lane
// picture on, with alpha 0 where a ring stands and `margin` metres either side of it. Floor elements
// that read this picture in place of the stage's lane read those places as open water, so nothing
// stands in a ring. The clearings are fixed to the floor. Render it 256 x 1.
// in0: the stage's lane picture (its `lane` output); blue is the metres per beat.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param at1 float = 0 0..20000 -- where the first ring stands, in beats of `travelled`; 0 none
// @param at2 float = 0 0..20000 -- the second ring
// @param at3 float = 0 0..20000 -- the third ring
// @param radius float = 5 1..20 -- the ring's radius, metres
// @param margin float = 2 0..20 -- metres of open water before and after the ring

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

fn inside(y: f32, at: f32, step: f32) -> bool {
    return at > 0.0 && abs(y - (at - params.beats) * step) <= params.radius + params.margin;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let i = i32(v.pos.x);
    let c = textureLoad(in0, vec2i(i, 0), 0);
    let y = LANE_NEAR + (f32(i) + 0.5) * (LANE_FAR - LANE_NEAR) / 256.0;
    let step = max(c.b, 0.01);
    if inside(y, params.at1, step) || inside(y, params.at2, step) || inside(y, params.at3, step) {
        return vec4f(c.rgb, 0.0);
    }
    return c;
}
