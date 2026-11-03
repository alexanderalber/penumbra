// The rocks of mid_debris, one per texel, drawn by geo.tex_instance with a low-poly rock. They
// come at the camera, `speed` times as fast as the floor, and pass it on both sides, never
// nearer than `clear` metres to the lane's middle or to the camera. Each pass of a rock takes a
// new place, size and side, from a hash of the rock, the pass and `seed`.
// in0: the stage's lane picture (its `lane` output). Unconnected: a straight lane at x = 0 and
// `step` metres per beat.
// in1: the burst's state (burst.wgsl). The rock that bursts is left out for the rest of its pass.
// Out: x, y, z of the rock's centre in rgb, its size in alpha (0 where none flies; the place is
// written all the same, so the shards can follow a burst rock). sizes.wgsl turns that into the
// rocks' scale. Render it 8 x 8.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param presence float = 1 0..1 -- 1 all rocks, towards 0 they shrink away one after another, 0 none
// @param count int = 40 1..64 -- rocks in flight
// @param seed int = 0 0..1000 -- another seed, other places
// @param speed float = 1.8 0.5..8 -- how much faster than the floor the rocks come
// @param size float = 0.35 0.05..1.5 -- a rock's mean size across, metres
// @param spread float = 3.5 0..10 -- metres beyond `clear` the rocks spread out sideways
// @param clear float = 1 0.3..4 -- metres a rock keeps from the lane's middle and from the camera
// @param low float = 0.8 0..4 -- the lowest a rock flies, metres (the camera is 1.1 to 2.9 m up)
// @param high float = 3.6 0..4 -- the highest, metres (the ceiling is 4 m up)
// @param near float = -2 -8..0 -- where a rock leaves, behind the camera
// @param far float = 14 4..56 -- where it appears
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

fn rand(i: u32, lap: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(i, lap, k + 97u * u32(max(params.seed, 0))))) / 4294967296.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let i = cell.y * u32(pn.resolution.x) + cell.x;
    let span = max(params.far - params.near, 1.0);
    let at = params.beats * step * params.speed / span + rand(i, 0u, 1u).x;
    let lap = u32(i32(floor(at)) & 0x7fffffff);
    let y = params.far - fract(at) * span;
    let r = rand(i, lap, 2u);
    let q = rand(i, lap, 3u);
    let size = params.size * mix(0.45, 1.6, r.x * r.x);
    // The lane where the rock is now; the rock keeps clear of it and of the camera at x = 0.
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    let off = params.clear + 0.5 * size + params.spread * r.y;
    let x = select(min(lane, 0.0) - off, max(lane, 0.0) + off, r.z < 0.5);
    let lo = params.low + 0.5 * size;
    let hi = max(params.high - 0.5 * size, lo);
    let z = mix(lo, hi, q.x);
    // Presence: each rock has its own share of it and shrinks away as presence falls below it.
    let own = clamp((clamp(params.presence, 0.0, 1.0) * 1.15 - q.y) * 8.0, 0.0, 1.0);
    var s = size * own * own * (3.0 - 2.0 * own);
    if i >= u32(max(params.count, 0)) || params.presence < 0.001 {
        s = 0.0;
    }
    // The burst rock: left out until its pass ends.
    let burst = textureLoad(in1, vec2i(0, 0), 0);
    if u32(burst.r + 0.5) == i + 1u && burst.b * step * params.speed < burst.g - params.near {
        s = 0.0;
    }
    return vec4f(x, y, z, s);
}
