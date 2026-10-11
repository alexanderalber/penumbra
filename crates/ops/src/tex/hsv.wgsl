// tex.hsv: hue turned, saturation and value scaled, on straight colour (the linear values);
// alpha kept.

fn rgb_to_hsv(c: vec3f) -> vec3f {
    let hi = max(c.r, max(c.g, c.b));
    let lo = min(c.r, min(c.g, c.b));
    let d = hi - lo;
    var h = 0.0;
    if d > 0.0 {
        if hi == c.r {
            h = (c.g - c.b) / d;
        } else if hi == c.g {
            h = 2.0 + (c.b - c.r) / d;
        } else {
            h = 4.0 + (c.r - c.g) / d;
        }
    }
    let s = select(0.0, d / hi, hi > 0.0);
    return vec3f(fract(h / 6.0), s, hi);
}

fn hsv_to_rgb(c: vec3f) -> vec3f {
    let k = fract(vec3f(c.x) + vec3f(1.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0;
    let p = clamp(abs(k) - 1.0, vec3f(0.0), vec3f(1.0));
    return c.z * mix(vec3f(1.0), p, c.y);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    var hsv = rgb_to_hsv(c.rgb);
    hsv.x = fract(hsv.x + params.hue / 360.0);
    hsv.y = clamp(hsv.y * params.saturation, 0.0, 1.0);
    hsv.z = max(hsv.z * params.value, 0.0);
    return vec4f(hsv_to_rgb(hsv) * c.a, c.a);
}
