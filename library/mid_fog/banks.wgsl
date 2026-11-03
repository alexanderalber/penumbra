// The fog banks of mid_fog, one per texel, drawn by geo.tex_instance with a hatched plane. They
// stand still over the floor, ahead on the lane, so the camera flies through them; each pass of
// a bank takes a new place from a hash. They drift sideways slowly.
// in0: the stage's lane picture (its `lane` output). Unconnected: a straight lane at x = 0 and
// `step` metres per beat.
// Out: x, y, z of the bank's middle in rgb, alpha the bank's own share of `presence` (0 where
// none stands). look.wgsl turns that into scale and light. Render it 4 x 3.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param presence float = 1 0..1 -- 1 all banks, towards 0 they fade one after another, 0 none
// @param count int = 8 1..12 -- banks along the span
// @param seed int = 0 0..1000 -- another seed, other places
// @param spread float = 1.5 0..6 -- metres a bank stands beside the lane's middle, at most
// @param low float = 0.8 0..4 -- the lowest a bank's middle stands, metres
// @param high float = 2.6 0..4 -- the highest, metres (the camera is 1.1 to 2.9 m up)
// @param sway float = 0.6 0..3 -- metres a bank drifts sideways while it comes nearer
// @param near float = -1 -8..2 -- where a bank leaves, behind the camera
// @param far float = 8 4..56 -- where it appears
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

fn rand(i: u32, lap: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(i, lap, k + 131u * u32(max(params.seed, 0))))) / 4294967296.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let cell = vec2u(v.pos.xy);
    let i = cell.y * u32(pn.resolution.x) + cell.x;
    let n = u32(max(params.count, 1));
    let span = max(params.far - params.near, 1.0);
    // Evenly spaced along the span with a little jitter, moving with the floor.
    let at = params.beats * step / span + (f32(i) + 0.4 * rand(i, 0u, 1u).x) / f32(n);
    let lap = u32(i32(floor(at)) & 0x7fffffff);
    let f = fract(at);
    let y = params.far - f * span;
    let r = rand(i, lap, 2u);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    let x = lane + (2.0 * r.x - 1.0) * params.spread + params.sway * (f - 0.5) * (2.0 * r.z - 1.0);
    let z = mix(params.low, max(params.high, params.low), r.y);
    let own = clamp((clamp(params.presence, 0.0, 1.0) * 1.15 - rand(i, lap, 3u).x) * 6.0, 0.0, 1.0);
    if i >= n || params.presence < 0.001 {
        return vec4f(x, y, z, 0.0);
    }
    return vec4f(x, y, z, own * own * (3.0 - 2.0 * own));
}
