// tex.monochrome: grey from luminance (Rec. 709), the average, the brightest channel or one
// channel, on straight colour; alpha kept or made opaque.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    var g: f32;
    switch params.method {
        case method_average: { g = (c.r + c.g + c.b) / 3.0; }
        case method_maximum: { g = max(c.r, max(c.g, c.b)); }
        case method_red: { g = c.r; }
        case method_green: { g = c.g; }
        case method_blue: { g = c.b; }
        case method_alpha: { g = c.a; }
        default: { g = pn_luma(c.rgb); }
    }
    let a = select(c.a, 1.0, params.opaque != 0u);
    return vec4f(vec3f(g * a), a);
}
