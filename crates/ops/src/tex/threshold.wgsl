// tex.threshold: where the chosen channel (straight colour) is at least `threshold`, with a soft
// ramp `soft` wide above it. `mask` gives white on black, `alpha` keeps the input where it
// passes and makes the rest transparent.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let c = pn_unpremultiply(src);
    var x: f32;
    switch params.channel {
        case channel_red: { x = c.r; }
        case channel_green: { x = c.g; }
        case channel_blue: { x = c.b; }
        case channel_alpha: { x = c.a; }
        default: { x = pn_luma(c.rgb); }
    }
    var m = select(step(params.threshold, x), smoothstep(params.threshold, params.threshold + params.soft, x), params.soft > 0.0);
    if (params.invert != 0u) {
        m = 1.0 - m;
    }
    if (params.output == output_alpha) {
        return src * m;
    }
    return vec4f(vec3f(m), 1.0);
}
