// tex.displace: each pixel of `in` (in0) is fetched from somewhere else, pushed by the `map`
// (in1): the chosen channel minus `midpoint`, times `weight` (in image sizes).

fn pn_pick(c: vec4f, which: i32) -> f32 {
    switch which {
        case 0: { return c.r; }
        case 1: { return c.g; }
        case 2: { return c.b; }
        case 3: { return c.a; }
        case 4: { return pn_luma(c.rgb); }
        default: { return params.midpoint; }
    }
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let m = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    let push = vec2f(pn_pick(m, params.horizontal), pn_pick(m, params.vertical)) - params.midpoint;
    return pn_extend(in0, v.uv + push * params.weight, params.extend);
}
