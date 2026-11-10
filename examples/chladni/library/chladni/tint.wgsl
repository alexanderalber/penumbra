// What each grain adds into `deposit`: red counts the grain, green how far it moved per step
// lately (a moving grain counts up to 1). in0: the grains.
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let s = textureLoad(in0, vec2i(v.pos.xy), 0);
    return vec4f(1.0, clamp(s.a * 0.5, 0.0, 1.0), 0.0, 1.0);
}
