// tex.erode and tex.dilate: one axis of a rectangular minimum (erode) or maximum (dilate), per
// channel of the stored colour. The node runs it once across and once down, `axis` and `grow`
// set by the node, not user params. A fractional radius blends the outermost pixel in by its
// fraction, so a size bound to a channel grows smoothly.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let radius = clamp(dot(params.size, params.axis), 0.0, 256.0);
    var m = pn_extend(in0, v.uv, params.extend);
    if (radius <= 0.0) {
        return m;
    }
    let stp = params.axis / vec2f(textureDimensions(in0));
    let grow = params.grow > 0.5;
    let n = i32(ceil(radius));
    for (var i = 1; i <= n; i += 1) {
        let o = f32(i) * stp;
        let a = pn_extend(in0, v.uv + o, params.extend);
        let b = pn_extend(in0, v.uv - o, params.extend);
        let t = select(min(m, min(a, b)), max(m, max(a, b)), grow);
        m = mix(m, t, min(radius - f32(i - 1), 1.0));
    }
    return m;
}
