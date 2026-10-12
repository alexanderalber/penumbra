// tex.text: places the rasterised text (in0, white coverage, premultiplied) so that its anchor
// sits at `position` (uv, y down), tinted with `color` (straight alpha, linear), scaled by
// `scale` (above 1 only while a raster for a new size is on its way).

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let dims = vec2f(textureDimensions(in0));
    if (dims.x < 1.0 || dims.y < 1.0) {
        return vec4f(0.0);
    }
    // Output pixel to text-texture pixel; the anchor is in text-texture pixels.
    let q = (v.uv - params.position) * pn.resolution / max(params.scale, 1e-6) + params.anchor;
    let uv = q / dims;
    if (any(uv < vec2f(0.0)) || any(uv > vec2f(1.0))) {
        return vec4f(0.0);
    }
    let coverage = textureSampleLevel(in0, pn_clamp, uv, 0.0).a;
    let a = params.color.a * coverage;
    return vec4f(params.color.rgb * a, a);
}
