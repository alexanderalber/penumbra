// Blocks on the floor of the synthwave stage, one per texel, drawn by geo.tex_instance. A column
// of texels is a street across (x), a row a place along it (y). The floor moves towards the
// camera: `beats` is the distance travelled in beats. Blocks lie flat until the first beat after
// they pass `rise_at`, then shoot out of the floor within `rise` beats, so a new row stands up
// on every beat. Low blocks keep the stage's lane free.
// in0: the stage's lane picture (its `lane` output): red the lane's x, blue the metres per beat,
// alpha the floor (0 in a gap), texel i covering y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres
// ahead. Unconnected: a straight lane at x = 0, no gaps and `step` metres per beat.
// Out: x, y, z of the block's centre in rgb, its side in alpha (0 where no block stands). A
// second node (sizes.wgsl) turns that into the blocks' scale.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the grid's tempo, smoothed and added up
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the blocks rise on its start
// @param beat_length float = 1 0.25..4 -- how far the world moves in one of the grid's beats, in beats of `beats` (beat_length of a chan.beat_grid)
// @param presence float = 1 0..1 -- how much of the city stands: 1 all of it, towards 0 the blocks sink back into the floor one after another, 0 none
// @param rise_at float = 8 0..50 -- metres ahead where they shoot out of the floor, on the next beat
// @param rise float = 0.25 0.05..2 -- beats a block takes to stand up
// @param lane float = 1.6 0..5 -- half the width of the low lane
// @param fill float = 0.7 0..1 -- share of places that hold a block
// @param spread float = 12 1..40 -- width of the city across
// @param near float = -1 -8..10 -- where the blocks leave, behind the camera
// @param far float = 11 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
// Metres a block's bottom stands above the floor.
const LIFT: f32 = 0.02;
const LANE_FAR: f32 = 56.0;

// Whether the floor stands under a thing `y` metres ahead: the lane picture's alpha, read as in
// or out. The floor breaks off in whole cells of one beat, so the test reads the middle of the
// thing's cell from one texel, without filtering. A thing on a gap's edge then stays in or out
// as it comes nearer, instead of flickering as a stub. A gap that is opening takes it at once.
fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = params.beats;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let size = pn.resolution;
    let span = params.far - params.near;
    let spacing = span / size.y;
    // Each street runs a little out of step with its neighbours.
    let street = f32(pn_hash(vec3u(cell.x, 7u, 0u)).x) / 4294967296.0;
    let ahead = (f32(cell.y) + street) * spacing;
    let travelled = c * step + ahead;
    let lap = floor(travelled / span);
    let y = params.far - (travelled - lap * span);
    // A new block on every lap: three random numbers for its side, place and height.
    let r = vec3f(pn_hash(vec3u(cell, u32(max(lap, 0.0))))) / 4294967296.0;
    let slot = params.spread / size.x;
    let side = min(mix(0.25, 0.5, r.x), 0.9 * min(slot, spacing));
    let x = -0.5 * params.spread + (f32(cell.x) + 0.5) * slot + (r.y - 0.5) * (slot - side);
    // The beat on which this block passed `far`.
    let born = (lap * span - ahead) / step;
    // The lane where the block stands. It is fixed to the floor, so it stays the same while the
    // block comes nearer.
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let lane_x = lane.r;
    // Low inside the lane, tall outside, with a soft edge: when the lane glides sideways after a
    // jump, the blocks at its edge grow or shrink instead of switching at once.
    let inside = 1.0 - smoothstep(params.lane - 0.3, params.lane + 0.3, abs(x - lane_x));
    var height = mix(mix(0.2, 1.4, r.z), mix(0.04, 0.24, r.z), inside);
    // Up on the first beat after passing `rise_at`, eased out over `rise` beats. `last` is about
    // how far the world had moved at the start of the grid's current beat. The world moves by
    // the smoothed tempo, so `last` can slip back a little within a beat that is shorter than
    // the tempo says. `slack` keeps a block up then.
    let slack = 0.2;
    let passing = born + max(params.far - params.rise_at, 0.0) / step - slack;
    let last = c - params.beat * params.beat_length;
    let first = fract(last);
    let up = first + ceil(passing - first);
    let g = select(0.0, clamp((c - up) / params.rise, 0.0, 1.0), passing <= last);
    height *= 1.0 - (1.0 - g) * (1.0 - g) * (1.0 - g);
    // Presence: each block has its own share of it, so as presence falls the blocks sink back
    // into the floor one after another. At 0.5 about half the city's height is left.
    let order = fract(r.y * 53.0 + r.z * 7.0);
    let own = clamp(params.presence * 2.0 - order, 0.0, 1.0);
    height *= own * own * (3.0 - 2.0 * own);
    // Where the floor has a gap, no block stands.
    let floor_here = !wired || on_floor(y, step);
    // A block under 3 cm is left out, and every block stands `LIFT` above the floor: a bottom face
    // in the floor's plane flickers against its lines.
    let present = fract(r.x * 97.0 + r.z * 13.0) < params.fill && g > 0.0 && own > 0.0 && height >= 0.03 && floor_here;
    if !present {
        return vec4f(x, y, 0.0, 0.0);
    }
    return vec4f(x, y, 0.5 * height + LIFT, side);
}
