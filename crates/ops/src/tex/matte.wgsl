// tex.matte: `a` (in0) where the matte (in2) is 1, `b` (in1) where it is 0, mixed in between,
// premultiplied. The matte's colour channels are read as they are (premultiplied), so a
// transparent pixel counts as 0 whatever its colour. `b` and the matte are stretched to the size
// of `a`. Without `b` the matte cuts `a` out (transparent where it is 0).

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let a = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let b = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    let m = textureSampleLevel(in2, pn_clamp, v.uv, 0.0);
    var x: f32;
    switch params.channel {
        case channel_red: { x = m.r; }
        case channel_green: { x = m.g; }
        case channel_blue: { x = m.b; }
        case channel_alpha: { x = m.a; }
        default: { x = pn_luma(m.rgb); }
    }
    x = clamp(x, 0.0, 1.0);
    if (params.invert != 0u) {
        x = 1.0 - x;
    }
    return mix(b, a, x);
}
