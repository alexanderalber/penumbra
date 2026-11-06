// The beams of mid_lasers, one per texel, for geo.tex_instance with a thin prism one unit long
// along y. The beams fan out from a point low over the lane `origin` metres ahead, where the
// floor fades into the distance, and pass the camera above and on both sides. The fan's middle
// sweeps from side to side once every two bars. A kick takes every beam out for one frame.
// The stage may say where the point lies and how the fan opens (its `pose`): `axis` puts the
// point that high, and `round` 1 opens the fan all round the point, so in a tube the beams
// leave from its axis towards the wall all round and pass the camera on every side.
// in0: the stage's lane picture (its `lane` output); the fan's point follows the lane.
// Unconnected: a straight lane at x = 0.
// `what` place: the middle of each beam in rgb. aim: its direction. scale: `thickness` across,
// its length along y (0 for none). Render it 16 x 1.
// @param what choice(place|aim|scale) = place -- what this node writes
// @param bar_index float = 0 0..100000 -- bars counted (bar_index of a chan.beat_grid)
// @param bar float = 0 0..1 -- the bar's progress (bar of a chan.beat_grid)
// @param kick float = 0 0..1 -- 1 in the frame a kick starts (kick of a chan.beat_grid): the beams go dark for that frame
// @param presence float = 1 0..1 -- 1 all beams, towards 0 they draw back into their point one after another
// @param count int = 9 5..15 -- beams in the fan
// @param origin float = 10 4..30 -- metres ahead of the camera where the beams start
// @param low float = 0.5 0..2 -- metres above the floor where they start
// @param reach float = -1 -4..2 -- metres ahead of the camera where they end (below 0: behind it)
// @param across float = 6 1..12 -- how far out to the side the outer beams end, metres
// @param up float = 3.2 0.5..3.5 -- how high the middle beam ends above `low`, metres (the ceiling is 4 m up)
// @param open float = 55 5..80 -- degrees from the fan's middle to its outer beams
// @param sweep float = 20 0..45 -- degrees the fan's middle swings to each side
// @param thickness float = 0.015 0.001..0.1 -- metres across a beam; the line is drawn as thick as the wireframe's line_width at any size
// @param strobe bool = true -- a kick takes the beams out for one frame
// @param axis float = -1 -1..10 -- metres up where the beams start, from the stage (axis of its pose); below 0 `low` holds
// @param round float = 0 0..1 -- 0 a fan upwards, 1 a fan all round its point, as in a tube (curve of the stage's pose)

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// The farthest a beam leans from straight up, degrees: steeper ones would graze the floor.
const LEAN_MAX: f32 = 78.0;
// The highest a beam ends, metres (under the ceiling at 4 m).
const TOP: f32 = 3.8;

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let i = u32(v.pos.x);
    let n = u32(clamp(params.count, 5, 15));
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let u = (params.origin - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    let low = select(params.low, params.axis, params.axis >= 0.0);
    let start = vec3f(lane, params.origin, low);
    // The fan's middle: once to each side every two bars.
    let bars = params.bar_index + clamp(params.bar, 0.0, 1.0);
    let middle = params.sweep * sin(3.14159265 * bars);
    let k = f32(i) / f32(max(n - 1u, 1u));
    let lean = radians(clamp(middle + params.open * (2.0 * k - 1.0), -LEAN_MAX, LEAN_MAX));
    let up = min(params.up, max(TOP - low, 0.3));
    let flat = vec3f(params.across * sin(lean), params.reach, low + up * cos(lean));
    // All round: the beams spread evenly round the point and turn with the sweep, ending inside
    // a tube whose axis lies at `low`.
    let all_round = 6.2831853 * f32(i) / f32(max(n, 1u)) + radians(middle);
    let wide = min(params.across, 0.8 * max(low, 0.5));
    let ring = vec3f(wide * sin(all_round), params.reach, low + wide * cos(all_round));
    let end = mix(flat, ring, clamp(params.round, 0.0, 1.0));
    let full = end - start;
    let dir = normalize(full);
    // Presence: the middle beams stay longest; each one draws back into the point as presence
    // falls below its share.
    let from_middle = abs(2.0 * k - 1.0);
    let own = clamp((clamp(params.presence, 0.0, 1.0) * 1.1 - 0.9 * from_middle) * 5.0, 0.0, 1.0);
    var len = length(full) * own * own * (3.0 - 2.0 * own);
    if i >= n || params.presence < 0.001 || (params.strobe != 0u && params.kick > 0.5) {
        len = 0.0;
    }
    if params.what == what_aim {
        return vec4f(dir, 1.0);
    }
    if params.what == what_scale {
        let t = select(0.0, params.thickness, len > 0.0);
        return vec4f(t, len, t, 1.0);
    }
    return vec4f(start + 0.5 * len * dir, 1.0);
}
