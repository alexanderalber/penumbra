// The start state of `step` (see step.wgsl for the channels). Its width and height are the
// simulation's size, from the component. The component's reset starts again from here.
//
// in0: the object mask, any size, for the pattern `edges`.
//
// @param seed int = 1 0..1000 -- follows the component's seed: other random places and colours
// @param model choice(frost|branch|lichen) = frost -- follows the component's model: what the state is written for
// @param pattern choice(centre|scatter|edges|colonies|bottom|empty) = empty -- follows the component's pattern: where growth starts
// @param count int = 12 1..200 -- follows the component's count: seeds of the patterns scatter and colonies
// @param radius float = 4 1..40 -- follows the component's radius: seed radius in texels

fn rand(i: u32, k: u32) -> f32 {
    let h = pn_hash(vec3u(i, k, u32(params.seed) * 7919u + 17u));
    return f32(h.x >> 8u) / 16777216.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let model = params.model;
    let pattern = params.pattern;
    let count = params.count;

    let n = pn.resolution;
    let p = floor(v.pos.xy) + 0.5;
    // Which seed is this texel in? 0 for none, else 1 + a random number (colour, orientation).
    var s = 0.0;
    if pattern == pattern_centre {
        if length(p - n * 0.5) <= params.radius { s = 1.0 + rand(0u, 3u); }
    } else if pattern == pattern_scatter || pattern == pattern_colonies {
        for (var i = 0; i < count; i++) {
            let c = vec2f(0.05 + 0.9 * rand(u32(i), 1u), 0.05 + 0.9 * rand(u32(i), 2u)) * n;
            if length(p - c) <= params.radius { s = 1.0 + rand(u32(i), 3u); }
        }
    } else if pattern == pattern_edges {
        let o = 1.5 / n;
        let a = pn_luma(textureSampleLevel(in0, pn_clamp, v.uv + vec2f(o.x, 0.0), 0.0).rgb);
        let b = pn_luma(textureSampleLevel(in0, pn_clamp, v.uv - vec2f(o.x, 0.0), 0.0).rgb);
        let c = pn_luma(textureSampleLevel(in0, pn_clamp, v.uv + vec2f(0.0, o.y), 0.0).rgb);
        let d = pn_luma(textureSampleLevel(in0, pn_clamp, v.uv - vec2f(0.0, o.y), 0.0).rgb);
        if max(max(a, b), max(c, d)) > 0.5 && min(min(a, b), min(c, d)) < 0.5 {
            let blk = vec2u(p / 3.0);
            s = 1.0 + rand(blk.x * 7919u + blk.y, 4u);
        }
    } else if pattern == pattern_bottom {
        if p.y > n.y - 2.0 { s = 1.0 + rand(u32(p.x / 40.0), 5u); }
    }

    var r = vec4f(0.0, 0.0, 0.0, 1.0);
    if model == model_frost {
        // g is the temperature: everything starts cold (T = 0). b 0: frozen at the start.
        r = select(vec4f(0.0, 0.0, 0.0, 1.0), vec4f(1.0, 0.0, 0.0, s), s > 0.0);
    } else if model == model_branch {
        r = select(vec4f(0.0, 1.0, 0.0, 1.0), vec4f(1.0, 0.0, 0.0, 1.0), s > 0.0);
    } else {
        r = select(vec4f(0.0, 0.0, 0.0, 1.0), vec4f(0.02 + 0.98 * (s - 1.0), rand(u32(p.x) * 4099u + u32(p.y), 6u), 0.0, 1.0), s > 0.0);
    }
    return r;
}
