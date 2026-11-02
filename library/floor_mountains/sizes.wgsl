// The peaks' scale from their places (mountains.wgsl in in0): the width in alpha becomes x,
// `length` times it y, twice the centre's height z. Render it at the size of in0.
// @param length float = 1.4 0.5..4 -- how much longer a peak is along the lane than across it
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = textureLoad(in0, vec2i(v.pos.xy), 0);
    return vec4f(p.a, p.a * params.length, 2.0 * p.z, 1.0);
}
