// tex.sequence: `a` (in0) mixed into `b` (in1) by `amount`; a side without a picture is the
// colour `color_a` or `color_b` (black or white, premultiplied). `b` is stretched to the size of
// the picture made.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    var a = params.color_a;
    if params.has_a > 0.5 {
        a = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    }
    var b = params.color_b;
    if params.has_b > 0.5 {
        b = textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    }
    return mix(a, b, clamp(params.amount, 0.0, 1.0));
}
