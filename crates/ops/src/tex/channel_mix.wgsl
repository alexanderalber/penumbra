// tex.channel_mix: each output channel is a weighted sum of the input's red, green, blue and
// alpha plus a bias, on straight colour.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    let out = vec4f(
        dot(params.red, c),
        dot(params.green, c),
        dot(params.blue, c),
        dot(params.alpha, c),
    ) + params.bias;
    let a = clamp(out.a, 0.0, 1.0);
    return vec4f(out.rgb * a, a);
}
