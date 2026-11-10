// The start of a Lenia run: random patches of cells, or copies of a creature, on an empty field.
// Written as the state texture of step.wgsl: r, g, b = the living channels, a = 1. Its size is
// in0's (the component's world). Sizes are in units of the kernel radius, so the start looks the
// same at any simulation size.
//
// in0: the world (only its size counts).
// in1: the creatures: square slots side by side (glider, ring), each for a radius of 13 texels.
// in2: fields.wgsl: r the mask (start mask starts cells only where it is bright).
// in3: guard.wgsl: texel 2 a counts the restarts by the guard, so each one starts differently.
//
// @param start choice(patches|noise|one|stamps|mask|empty) = stamps -- follows the component's start
// @param size float = 6.15 0.5..20 -- follows the component's size
// @param density float = 0.5 0..1 -- follows the component's density
// @param creature choice(none|glider|ring) = glider -- follows the component's creature
// @param rule choice(lenia|smoothlife|three) = lenia -- follows the component's rule; three starts each patch as one of three kinds
// @param seed int = 1 0..1000 -- another seed gives other patches and places
// @param scale float = 1 0.25..4 -- follows the component's scale
// @param radius float = 17.33 4..40 -- follows the component's radius

fn rnd(p: vec2u, k: u32) -> vec3f {
    let variant = u32(textureLoad(in3, vec2i(2, 0), 0).a);
    let h = pn_hash(vec3u(p, k + u32(params.seed) * 7919u + variant * 104729u));
    return vec3f(f32(h.x >> 8u), f32(h.y >> 8u), f32(h.z >> 8u)) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = pn.resolution;
    let p = floor(v.pos.xy);
    let pat = params.start;
    let radius = params.radius * params.scale * n.y / 288.0;
    // size is in kernel radii.
    let s = max(params.size * radius, 2.0);
    let cell = floor(p / s);
    let cu = vec2u(vec2i(cell) + vec2i(10000));
    let pick = rnd(cu, 3u);
    var a = vec3f(0.0);
    if pat == start_stamps && params.creature != creature_none {
        let dim = vec2f(textureDimensions(in1));
        let slots = max(floor(dim.x / dim.y), 1.0);
        let slot = f32(params.creature - 1);
        let sz = vec2f(48.0 * radius / 13.0);
        let room = max(vec2f(s) - sz, vec2f(0.0));
        let jit = rnd(cu, 5u);
        let centre = cell * s + sz * 0.5 + jit.xy * room;
        let ang = jit.z * 6.2831853;
        let d = p + 0.5 - centre;
        let local = vec2f(cos(ang) * d.x + sin(ang) * d.y, -sin(ang) * d.x + cos(ang) * d.y);
        let uv = local / sz + 0.5;
        if pick.x < params.density && all(uv > vec2f(0.0)) && all(uv < vec2f(1.0)) {
            let au = vec2f((slot + uv.x) / slots, uv.y);
            a = textureSampleLevel(in1, pn_clamp, au, 0.0).rgb;
        }
    } else {
        var on = false;
        if pat == start_patches || pat == start_stamps {
            on = pick.x < params.density;
        } else if pat == start_mask {
            on = pick.x < params.density && textureSampleLevel(in2, pn_clamp, v.uv, 0.0).r > 0.5;
        } else if pat == start_noise {
            on = true;
        } else if pat == start_one {
            let d = abs(p - floor(n * 0.5));
            on = max(d.x, d.y) < s * 0.5;
        }
        if on {
            let c = rnd(vec2u(p), 17u);
            var mask = vec3f(1.0, 0.0, 0.0);
            if params.rule == rule_three {
                // Each patch gets one channel.
                let ch = floor(pick.y * 3.0);
                mask = vec3f(select(0.0, 1.0, ch == 0.0), select(0.0, 1.0, ch == 1.0), select(0.0, 1.0, ch == 2.0));
            }
            a = c * mask;
        }
    }
    return vec4f(a, 1.0);
}
