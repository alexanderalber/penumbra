// chan.from_tex: one step of averaging the input down, the mean over each output pixel's
// footprint, as stored (premultiplied, alpha kept).

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let footprint = vec2f(textureDimensions(in0)) * pn.texel;
    return pn_area(in0, pn_clamp, v.uv, footprint);
}
