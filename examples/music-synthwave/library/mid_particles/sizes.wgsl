// The streaks' scale from their places (streaks.wgsl in in0): `thickness` across, the length in
// alpha along y. Render it at the size of in0.
// @param thickness float = 0.01 0.001..0.1 -- metres across a streak; the line is drawn as thick as the wireframe's line_width at any size
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = textureLoad(in0, vec2i(v.pos.xy), 0);
    let t = select(0.0, params.thickness, p.a > 0.0);
    return vec4f(t, p.a, t, 1.0);
}
