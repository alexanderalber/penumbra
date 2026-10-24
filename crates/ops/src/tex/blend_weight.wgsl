// tex.blend_weight: in0 the picture, in1 its large features (the picture blurred, or the picture
// itself in smooth mode), in2 the blend weight, in3 the detail weight. The large features are
// weighted by the blend weight, the rest by the detail weight; no light below zero.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let low = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    let w = textureSampleLevel(in2, pn_clamp, v.uv, 0.0);
    let d = textureSampleLevel(in3, pn_clamp, v.uv, 0.0);
    let o = low * w + (c - low) * d;
    return vec4f(max(o.rgb, vec3f(0.0)), clamp(o.a, 0.0, 1.0));
}
