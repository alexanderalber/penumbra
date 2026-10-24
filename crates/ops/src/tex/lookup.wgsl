// tex.lookup: the input's values pick colours from the `lut` (in1), read left (0) to right (1)
// along the row at height `row`, or from `palette` when one is set. `luminance` looks up one
// colour per pixel, `channels` each channel in its own channel of the lut (a per-channel
// curve). On straight colour.

fn pn_lut(x: f32) -> vec4f {
    if (params.palette.count > 0) {
        if (params.steps != 0u) {
            return pn_band(params.palette, x);
        }
        return pn_colormap(params.palette, x);
    }
    let w = f32(textureDimensions(in1).x);
    // Texel centres at the ends, so 0 and 1 hit the first and last entry exactly.
    let u = (clamp(x, 0.0, 1.0) * (w - 1.0) + 0.5) / w;
    return pn_unpremultiply(textureSampleLevel(in1, pn_clamp, vec2f(u, params.row), 0.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = pn_unpremultiply(textureSampleLevel(in0, pn_clamp, v.uv, 0.0));
    var out: vec4f;
    if (params.index == index_channels) {
        out = vec4f(pn_lut(c.r).r, pn_lut(c.g).g, pn_lut(c.b).b, c.a);
    } else {
        let l = pn_lut(pn_luma(c.rgb));
        out = vec4f(l.rgb, l.a * c.a);
    }
    return vec4f(out.rgb * out.a, out.a);
}
