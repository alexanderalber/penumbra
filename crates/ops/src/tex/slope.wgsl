// tex.slope: how steeply the chosen channel rises, left to right in red and top to bottom in
// green (per pixel, times `strength`, plus `offset`, so a flat image is 0.5 grey with the
// default), blue 0, opaque. Feeds tex.displace for refraction-like effects.

fn pn_val(uv: vec2f) -> f32 {
    let c = textureSampleLevel(in0, pn_clamp, uv, 0.0);
    switch params.channel {
        case channel_red: { return c.r; }
        case channel_green: { return c.g; }
        case channel_blue: { return c.b; }
        case channel_alpha: { return c.a; }
        default: { return pn_luma(c.rgb); }
    }
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let t = pn.texel;
    let dx = (pn_val(v.uv + vec2f(t.x, 0.0)) - pn_val(v.uv - vec2f(t.x, 0.0))) * 0.5;
    let dy = (pn_val(v.uv + vec2f(0.0, t.y)) - pn_val(v.uv - vec2f(0.0, t.y))) * 0.5;
    return vec4f(vec2f(dx, dy) * params.strength + params.offset, 0.0, 1.0);
}
