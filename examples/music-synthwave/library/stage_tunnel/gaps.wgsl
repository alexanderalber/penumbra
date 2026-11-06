// The tube's gaps as they were when each part of the tube came into view. A change of `gap`
// (the arrangement's share of the tube in gaps) reaches only the tube beyond `far`, where the
// fade hides it. So a gap's edge forms out of sight and comes towards the camera with the tube,
// instead of a stretch of tube vanishing in front of it.
// Render it 512 x 1 and feed it back into itself through a tex.feedback (in0, the previous
// frame). Texel i holds cell n (one beat of tube) for the n with n = i modulo 512 that lies
// in a window from 192 cells behind the camera onwards:
//   red    the share of the tube in gaps when that cell was last beyond `far`
//   green  the window's lap the cell belongs to, in sixteenths: a texel from another lap (after
//          a seek, or on the first frame) takes the current `gap` at once
// The lane (lane.wgsl) reads red at the first cell of each stretch of the tube.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param step float = 0.5 0.1..2 -- metres the world moves per beat
// @param gap float = 0 0..1 -- share of the tube that runs out into gaps now
// @param far float = 10 1..100 -- metres ahead beyond which the tube is out of sight (the fade's end)

const CELLS: f32 = 512.0;
const BEHIND: f32 = 192.0;
const LAPS: f32 = 16.0;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let i = floor(v.pos.x);
    let first = floor(params.beats) - BEHIND;
    // The cell this texel holds: the one in [first, first + CELLS) that is i modulo CELLS.
    let n = first + (i - first - CELLS * floor((i - first) / CELLS));
    let lap = floor(n / CELLS);
    let tag = (lap - LAPS * floor(lap / LAPS) + 0.5) / LAPS;
    let gap = clamp(params.gap, 0.0, 1.0);
    let ahead = (n - params.beats) * max(params.step, 0.01);
    let before = textureLoad(in0, vec2i(i32(i), 0), 0);
    if ahead >= params.far || abs(before.g - tag) > 0.01 {
        return vec4f(gap, tag, 0.0, 1.0);
    }
    return before;
}
