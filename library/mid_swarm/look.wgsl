// Place, heading or scale per agent of the swarm, from its state (boids.wgsl in in0), for
// geo.tex_instance. Copy i is agent i. `scale`: towards `presence` 0 the agents shrink away one
// after another. Render it 16 x 16.
// @param what choice(place|aim|scale) = place -- what this node writes
// @param presence float = 1 0..1 -- 1 the whole swarm, towards 0 it thins out, 0 none
// @param agents int = 200 1..256 -- agents in the swarm (boids.wgsl's)
// @param at float = 6 4.5..9 -- metres ahead where the swarm flies (boids.wgsl's)
// @param hover float = 2.2 0.5..3.5 -- metres above the floor (boids.wgsl's)
// @param size float = 0.12 0.02..0.5 -- an agent's length, metres
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let i = cell.y * 16u + cell.x;
    let p = textureLoad(in0, vec2i(vec2u(2u * cell.x, cell.y)), 0);
    let vel = textureLoad(in0, vec2i(vec2u(2u * cell.x + 1u, cell.y)), 0);
    if params.what == what_place {
        return vec4f(p.xyz + vec3f(0.0, params.at, params.hover), 1.0);
    }
    if params.what == what_aim {
        // Straight up or down has no heading to keep +z up by, so the nose stays at most 70
        // degrees from level.
        let sp = length(vel.xyz);
        var d = select(vec3f(0.0, 1.0, 0.0), vel.xyz / max(sp, 1e-4), sp > 1e-3);
        let level = length(d.xy);
        if level < 0.34 {
            let flat = select(vec2f(0.0, 1.0), d.xy / max(level, 1e-4), level > 1e-3);
            d = vec3f(0.34 * flat, select(-0.94, 0.94, d.z > 0.0));
        }
        return vec4f(d, 1.0);
    }
    let r = vec3f(pn_hash(vec3u(i, 3u, 41u))) / 4294967296.0;
    let own = clamp((clamp(params.presence, 0.0, 1.0) * 1.15 - r.x) * 8.0, 0.0, 1.0);
    let on = select(0.0, 1.0, i < u32(clamp(params.agents, 1, 256)) && vel.a > 0.5 && params.presence > 0.001);
    let s = params.size * mix(0.8, 1.2, r.y) * own * own * (3.0 - 2.0 * own) * on;
    return vec4f(s, s, s, 1.0);
}
