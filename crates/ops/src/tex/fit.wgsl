// tex.fit: the input placed into a frame of `width` x `height`: whole and as large as fits
// (`fit`), covering the frame and cropped (`fill`), stretched to it, or pixel for pixel
// (`native`). `justify` places what does not fill the frame: 0 left or top, 1 right or bottom.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = vec2f(textureDimensions(in0));
    let out = pn.resolution;
    let ratio = out / src;
    var s: vec2f;
    switch params.fit {
        case fit_fill: { s = vec2f(max(ratio.x, ratio.y)); }
        case fit_stretch: { s = ratio; }
        case fit_native: { s = vec2f(1.0); }
        default: { s = vec2f(min(ratio.x, ratio.y)); }
    }
    let shown = src * s;
    let offset = (out - shown) * params.justify;
    let uv = (v.uv * out - offset) / shown;
    if (params.extend == extend_zero && (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0)))) {
        return vec4f(0.0);
    }
    var sampled: vec4f;
    switch params.extend {
        case extend_repeat: { sampled = pn_area(in0, pn_repeat, uv, 1.0 / s); }
        case extend_mirror: { sampled = pn_area(in0, pn_mirror, uv, 1.0 / s); }
        default: { sampled = pn_area(in0, pn_clamp, uv, 1.0 / s); }
    }
    return sampled;
}
