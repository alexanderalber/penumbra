// Reeds at the water's edge beside the lane of the valley stage, drawn by one geo.tex_instance of
// a clump (reeds.obj, reeds.py). One texel per clump: a column is a line of clumps along the lane
// (left lines first, the line at the lane's edge then the one behind it, then the right ones),
// a row a clump along it. The clumps stand about `spacing` metres apart, fixed to the floor, a
// few left out at random. They sway across the lane, a wave running along the line towards the
// camera, wider with the bass. Their tips glint in the swell.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the shore (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// `what` place: the clump's foot (rgb). aim: its turn and lean. scale: its size (0 where none
// stands). color: the glint, as a factor on the reeds' ramp. Render each 4 x 72.
// @param what choice(place|aim|scale|color) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): the reeds sway wider with it
// @param swell float = 0 0..1 -- the stage's swell: the tips glint with it
// @param presence float = 1 0..1 -- how many reeds stand: towards 0 the clumps shrink into the water one after another, 0 none
// @param spacing float = 0.55 0.2..3 -- metres between two clumps of a line
// @param fill float = 0.8 0..1 -- share of the places that hold a clump
// @param size float = 1.2 0.3..3 -- a clump's height, metres, on average (the camera flies 0.9 to 2.4 m up)
// @param sway float = 7 0..30 -- degrees the reeds lean to either side at full bass
// @param calm float = 0.3 0..1 -- share of the sway left when the bass is quiet
// @param wave float = 6 1..40 -- metres between two crests of the sway along a line
// @param glint float = 3 0..10 -- how bright the tips glint at full swell
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param edge float = 0.2 0..4 -- metres between the lane's edge and the first line
// @param depth float = 0.7 0.1..4 -- metres between the first line and the second
// @param near float = -3 -8..10 -- where the reeds leave, behind the camera
// @param far float = 30 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

struct Clump {
    at: vec3f,
    aim: vec3f,
    size: vec3f,
    phase: f32,
}

fn clump(cell: vec2u) -> Clump {
    var c: Clump;
    c.size = vec3f(0.0);
    c.aim = vec3f(1.0, 0.0, 0.0);
    c.phase = 0.0;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let side = cell.x / 2u;
    let line = cell.x % 2u;
    if side > 1u {
        return c;
    }
    // Clump n of a line stands at y = spacing * n - the metres travelled; the whole and the
    // fraction of that count are kept apart to stay exact far into a file.
    let spacing = max(params.spacing, 0.05);
    let travel = params.beats * step / spacing + 0.37 * f32(line);
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / spacing + part);
    let k = first + f32(cell.y);
    let n = i32(whole + k);
    let r = vec4f(vec3f(pn_hash(vec3u(bitcast<u32>(n), cell.x, 71u))), f32(pn_hash(vec3u(bitcast<u32>(n), cell.x, 73u)).x)) / 4294967296.0;
    let y = spacing * (k - part + 0.6 * (r.x - 0.5));
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, side == 1u);
    let off = params.lane * (1.0 + 0.5 * abs(lane.g)) + params.edge + f32(line) * params.depth + 0.45 * params.depth * r.y;
    c.phase = r.z;
    // The sway: a lean across the lane, a wave running along the line towards the camera, one
    // crest every `wave` metres; each clump a little out of step.
    let strength = mix(clamp(params.calm, 0.0, 1.0), 1.0, clamp(params.bass, 0.0, 1.0));
    let along = y + params.beats * step;
    let wave = sin(6.2831853 * (along / max(params.wave, 0.5) + 0.25 * params.beats + 0.3 * r.z));
    let lean = radians(params.sway) * strength * wave + radians(4.0) * (r.w - 0.5);
    // aim: the clump's +y points across the lane, tipped down by `lean`, so its +z leans the
    // other way by as much.
    c.aim = vec3f(cos(lean), 0.0, -sin(lean));
    // Presence: each clump shrinks at its own share of it; at a steady presence it stands whole
    // or not at all.
    let own = clamp((params.presence * 1.6 - r.w) * 8.0, 0.0, 1.0);
    let up = own * own * (3.0 - 2.0 * own);
    c.at = vec3f(lane.r + sign * off, y, 0.0);
    let there = r.y * 0.999 + 0.0005 < params.fill;
    if y > params.far || up < 0.02 || !there || (wired && !on_floor(y, step)) {
        return c;
    }
    let h = params.size * mix(0.7, 1.3, r.x) * up;
    // Every other clump mirrored, so the clumps do not all look alike.
    let flip = select(1.0, -1.0, r.z < 0.5);
    c.size = vec3f(flip * h, h, h);
    return c;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = clump(vec2u(v.pos.xy));
    if params.what == what_place {
        return vec4f(c.at, 1.0);
    }
    if params.what == what_aim {
        return vec4f(c.aim, 1.0);
    }
    if params.what == what_scale {
        return vec4f(c.size, 1.0);
    }
    // The glint: faint without the swell, up to `glint` times the ramp's tip in it, each clump
    // catching the light at its own moment of the sway.
    let swell = clamp(params.swell, 0.0, 1.0);
    let flicker = 0.55 + 0.45 * sin(6.2831853 * (0.5 * params.beats + c.phase));
    return vec4f(vec3f(0.35 + params.glint * swell * flicker), 1.0);
}
