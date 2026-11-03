// The burst's state, one texel, kept from frame to frame by a tex.feedback (its output in in0,
// this node's output back into its `previous`). On a kick in a drop, at most once every `every`
// bars, the rock nearest `at` metres ahead bursts.
// in0: the state of the last frame. in1: the rocks (rocks.wgsl).
// Out: red the burst rock's number plus 1 (0 none yet), green how far ahead it burst (metres),
// blue the beats since the burst, alpha `beats` modulo 16 (to count the beats from frame to
// frame without losing precision). Render it 1 x 1.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param kick float = 0 0..1 -- 1 in the frame of a kick (kick of a chan.beat_grid)
// @param role float = 0 0..5 -- the section's role (section_role of a chan.beat_grid): bursts only in a drop, 3
// @param presence float = 1 0..1 -- bursts only while presence is above 0.5
// @param burst bool = true -- rocks burst on kicks in a drop; off: never
// @param every float = 2 0.25..16 -- bars at least between two bursts
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param at float = 5 2..10 -- metres ahead where a rock bursts best; rocks from 3.5 to 8 m qualify

const WRAP: f32 = 16.0;
// Beats the age stops counting at.
const AGE_CAP: f32 = 256.0;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let last = textureLoad(in0, vec2i(0, 0), 0);
    let now = params.beats - WRAP * floor(params.beats / WRAP);
    var d = now - last.a;
    d = d - WRAP * floor(d / WRAP);
    var state = vec4f(last.r, last.g, min(last.b + d, AGE_CAP), now);
    let gap = max(params.every, 0.25) * max(params.beats_per_bar, 1.0);
    let ready = params.burst != 0u && params.kick > 0.5 && abs(params.role - 3.0) < 0.5
        && params.presence > 0.5 && state.b > gap;
    if ready {
        let dims = textureDimensions(in1);
        var best = -1;
        var best_d = 1e9;
        var best_y = 0.0;
        for (var y = 0u; y < dims.y; y++) {
            for (var x = 0u; x < dims.x; x++) {
                let p = textureLoad(in1, vec2i(vec2u(x, y)), 0);
                let off = abs(p.y - params.at);
                if p.a > 0.05 && p.y > 3.5 && p.y < 8.0 && abs(p.x) < 4.5 && off < best_d {
                    best = i32(y * dims.x + x);
                    best_d = off;
                    best_y = p.y;
                }
            }
        }
        if best >= 0 {
            state = vec4f(f32(best + 1), best_y, 0.0, now);
        }
    }
    return state;
}
