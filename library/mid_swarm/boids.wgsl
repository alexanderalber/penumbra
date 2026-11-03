// The swarm's state, kept from frame to frame by a tex.feedback (its output in in0, this node's
// output back into its `previous`). Boids: each agent steers away from neighbours that come too
// close, matches their heading and keeps to their middle, and follows a leader that wanders
// along the lane `at` metres ahead. The build of a section pulls the swarm together; a drop
// scatters it. Agents stay above the floor, under the ceiling and ahead of the glider.
// Texel (2a, j) holds the place of agent j * 16 + a, texel (2a + 1, j) its velocity, both
// relative to the point `at` metres ahead and `hover` metres up, so half floats keep them exact
// enough. The place's alpha holds the show time modulo 2 s, to measure the frame's length. The
// velocity's alpha is 1 once the agent is set up.
// in0: the state of the last frame. in1: the stage's lane picture (its `lane` output).
// Render it 32 x 16.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the leader wanders with it
// @param build float = 0 0..1 -- the build before a boundary (build of a chan.beat_grid): pulls the swarm together
// @param drop float = 0 0..1 -- a drop (drop of a chan.beat_grid): scatters the swarm
// @param agents int = 200 1..256 -- agents in the swarm
// @param at float = 6 4.5..9 -- metres ahead where the swarm flies
// @param hover float = 2.2 0.5..3.5 -- metres above the floor
// @param wander float = 1.2 0..3 -- metres the leader wanders sideways
// @param speed float = 3 0.5..8 -- the agents' top speed, metres per second
// @param scatter float = 10 0..40 -- how hard a drop throws the agents apart

const SIDE: u32 = 16u;
const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// Metres within which an agent sees its neighbours, and keeps its distance.
const SEE: f32 = 0.9;
const KEEP: f32 = 0.45;
// The space the agents may fly in, absolute metres: above the floor, under the ceiling, ahead of
// the glider (3 m ahead, 1.2 m long each way) and not too far into the distance fade.
const FLOOR: f32 = 0.4;
const CEILING: f32 = 3.6;
const FRONT: f32 = 4.5;
const BACK: f32 = 9.0;

fn rand(a: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(a, k, 77u))) / 4294967296.0;
}

fn lane_x(y: f32) -> f32 {
    let lane0 = textureLoad(in1, vec2i(0, 0), 0);
    if lane0.b <= 0.0 {
        return 0.0;
    }
    return textureSampleLevel(in1, pn_clamp, vec2f((y - LANE_NEAR) / (LANE_FAR - LANE_NEAR), 0.5), 0.0).r;
}

// A push back into lo..hi, growing over the last half metre.
fn wall(x: f32, lo: f32, hi: f32) -> f32 {
    return max(lo + 0.5 - x, 0.0) - max(x - (hi - 0.5), 0.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let a = cell.x / 2u;
    let j = cell.y;
    let me = j * SIDE + a;
    let is_vel = (cell.x & 1u) == 1u;
    let now = pn_time.seconds - 2.0 * floor(pn_time.seconds / 2.0);
    let dims = textureDimensions(in0);
    let home = vec3f(0.0, params.at, params.hover);
    // Not set up yet: a place in a ball around the home point, a small random velocity.
    if dims.x != 2u * SIDE || dims.y != SIDE {
        return select(vec4f(1.2 * (rand(me, 1u) * 2.0 - 1.0), now), vec4f(0.5 * (rand(me, 2u) * 2.0 - 1.0), 1.0), is_vel);
    }
    let p0 = textureLoad(in0, vec2i(vec2u(2u * a, j)), 0);
    let v0 = textureLoad(in0, vec2i(vec2u(2u * a + 1u, j)), 0);
    if v0.a < 0.5 {
        return select(vec4f(1.2 * (rand(me, 1u) * 2.0 - 1.0), now), vec4f(0.5 * (rand(me, 2u) * 2.0 - 1.0), 1.0), is_vel);
    }
    var dt = now - p0.a;
    dt = dt - 2.0 * floor(dt / 2.0);
    dt = min(dt, 0.05);
    let p = p0.xyz;
    let vel = v0.xyz;
    let n = u32(clamp(params.agents, 1, 256));
    // Neighbours, and the whole swarm's middle.
    var near_p = vec3f(0.0);
    var near_v = vec3f(0.0);
    var count = 0.0;
    var apart = vec3f(0.0);
    var middle = vec3f(0.0);
    for (var k = 0u; k < n; k++) {
        let at = vec2u(2u * (k % SIDE), k / SIDE);
        let q = textureLoad(in0, vec2i(at), 0).xyz;
        middle += q;
        if k == me {
            continue;
        }
        let d = q - p;
        let dist = length(d);
        if dist < SEE && dist > 1e-4 {
            near_p += q;
            near_v += textureLoad(in0, vec2i(at + vec2u(1u, 0u)), 0).xyz;
            count += 1.0;
            if dist < KEEP {
                apart -= d / (dist * dist);
            }
        }
    }
    middle /= f32(n);
    // The leader wanders along the lane ahead.
    let b = params.beats;
    let lead_abs = vec3f(
        params.wander * sin(b * 0.21) + lane_x(params.at),
        params.at + 0.8 * sin(b * 0.13),
        params.hover + 0.5 * sin(b * 0.17),
    );
    let lead = lead_abs - home;
    let build = clamp(params.build, 0.0, 1.0);
    var acc = 1.0 * apart;
    if count > 0.0 {
        acc += 1.2 * (near_v / count - vel);
        acc += mix(0.4, 3.0, build) * (near_p / count - p);
    }
    let to_lead = lead - p;
    acc += mix(0.5, 2.0, build) * to_lead;
    // Circling the leader, about the axis along the lane.
    acc += 0.9 * vec3f(to_lead.z, 0.0, -to_lead.x);
    // A drop throws them out from the swarm's middle.
    let out = p - middle;
    acc += params.scatter * clamp(params.drop, 0.0, 1.0) * out / max(length(out), 0.2);
    let abs_p = p + home;
    acc += 12.0 * vec3f(0.0, wall(abs_p.y, FRONT, BACK), wall(abs_p.z, FLOOR, CEILING));
    var nv = vel + acc * dt;
    let top = params.speed * (1.0 + clamp(params.drop, 0.0, 1.0));
    let sp = length(nv);
    if sp > top {
        nv *= top / sp;
    } else if sp < 0.4 {
        nv = select(vec3f(0.0, 0.4, 0.0), nv * (0.4 / max(sp, 1e-4)), sp > 1e-4);
    }
    // Never out of the space, however hard the push.
    let lo = vec3f(-12.0, FRONT - 0.3, FLOOR - 0.2);
    let hi = vec3f(12.0, BACK + 0.5, CEILING + 0.2);
    let np = clamp(p + nv * dt + home, lo, hi) - home;
    if is_vel {
        return vec4f(nv, 1.0);
    }
    return vec4f(np, now);
}
