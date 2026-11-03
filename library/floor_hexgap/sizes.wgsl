// The parts' scale from their places (hexgap.wgsl in in0): `across` wide, the height in alpha
// tall; nothing where the height is 0. Render it at the size of in0.
// @param across float = 0.36 0.05..2 -- width of a pillar, in metres
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = textureLoad(in0, vec2i(v.pos.xy), 0);
    let w = select(0.0, params.across, p.a > 0.0);
    return vec4f(w, w, p.a, 1.0);
}
