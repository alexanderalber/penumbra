// tex.blob_track: the input shrunk to the size the tracker reads back (the mean over each output
// pixel's footprint), straight colour, opaque.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let footprint = vec2f(textureDimensions(in0)) * pn.texel;
    let c = pn_unpremultiply(pn_area(in0, pn_clamp, v.uv, footprint));
    return vec4f(c.rgb, 1.0);
}
