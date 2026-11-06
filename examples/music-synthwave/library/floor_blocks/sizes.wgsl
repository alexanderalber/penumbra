// The blocks' scale from their places (blocks.wgsl in in0): the side in alpha becomes x and y,
// twice the centre's height above its lift (blocks.wgsl's LIFT, 2 cm) becomes z. Render it at the size of in0.
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = textureLoad(in0, vec2i(v.pos.xy), 0);
    return vec4f(p.a, p.a, 2.0 * max(p.z - 0.02, 0.0), 1.0);
}
