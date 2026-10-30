// tex.resolution: the input at another resolution, fetched as `sampling` says. Linear averages
// every source texel a pixel covers when shrinking (up to 64 per axis), so fine patterns do not
// alias; growing it is bilinear.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let footprint = vec2f(textureDimensions(in0)) / pn.resolution;
    return pn_resample(in0, pn_clamp, v.uv, footprint, params.sampling);
}
