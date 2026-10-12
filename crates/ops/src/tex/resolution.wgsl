// tex.resolution: the input at another resolution. Shrinking averages every source texel a
// pixel covers (up to 16 per axis), so fine patterns do not alias; growing is bilinear.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let footprint = vec2f(textureDimensions(in0)) / pn.resolution;
    return pn_area(in0, pn_clamp, v.uv, footprint);
}
