// tex.level: input levels, gamma, contrast, brightness, invert and opacity, on straight colour.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    var rgb = (c.rgb - params.black) / max(params.white - params.black, 1e-6);
    rgb = pow(max(rgb, vec3f(0.0)), vec3f(1.0 / params.gamma));
    rgb = (rgb - 0.5) * params.contrast + 0.5;
    rgb = rgb * params.brightness;
    if (params.invert != 0u) {
        rgb = 1.0 - rgb;
    }
    let a = clamp(c.a * params.opacity, 0.0, 1.0);
    return vec4f(rgb * a, a);
}
