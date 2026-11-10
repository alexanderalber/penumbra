// The agents' start: one agent per texel. Set this node's size to the number of agents (width
// 1024, height the component's `agents`).
// State of an agent, 32-bit floats (format 32f):
//   r: x, g: y, in trail cells from the top left; b: heading in turns (0..1); a: 1 alive, -1 dead.
// in0: the world (its size is the simulation size). in1: fields.wgsl (a: where agents may be born).
// An agent that starts inside a wall is born again elsewhere in its first step.
//
// @param start choice(scatter|disc|ring|centre|line|food) = scatter -- where the agents start; scatter: everywhere; food: on the food input's bright spots
// @param radius float = 0.3 0.01..1 -- disc and ring: radius in picture heights
// @param seed int = 1 0..1000 -- another seed gives another start

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2u(v.pos.xy);
    let size = vec2f(textureDimensions(in0));
    let h = pn_hash(vec3u(p, u32(params.seed) * 7919u + 13u));
    let h2 = pn_hash(h);
    let r = vec4f(vec4u(h.x, h.y, h.z, h2.x) >> vec4u(8u)) / 16777216.0;
    let tau = 6.2831853;
    let pattern = params.start;
    var pos = r.xy * size;
    var ang = r.z * tau;
    var alive = 1.0;
    let c = 0.5 * size;
    let radius = params.radius;
    if pattern == start_disc {
        let a = r.x * tau;
        let d = sqrt(r.y) * radius * size.y;
        pos = c + d * vec2f(cos(a), sin(a));
        ang = a;
    } else if pattern == start_ring {
        let a = r.x * tau;
        let d = radius * size.y * (1.0 + 0.02 * (r.y - 0.5));
        pos = c + d * vec2f(cos(a), sin(a));
        ang = a + 0.5 * tau;
    } else if pattern == start_centre {
        pos = c + 2.0 * (r.xy - 0.5);
    } else if pattern == start_line {
        pos = vec2f(r.x * size.x, c.y + 4.0 * (r.y - 0.5));
    } else if pattern == start_food {
        alive = -1.0;
        var q = h2;
        for (var i = 0; i < 8; i++) {
            q = pn_hash(q);
            let at = vec2f(vec2u(q.xy) >> vec2u(8u)) / 16777216.0 * size;
            if textureLoad(in1, vec2i(at), 0).a > 0.05 {
                pos = at;
                alive = 1.0;
                break;
            }
        }
    }
    pos = pos - size * floor(pos / size);
    return vec4f(pos, fract(ang / tau), alive);
}
