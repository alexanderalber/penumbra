// What `sums` adds up to find the membrane's common timing and size: per membrane texel on the
// plate r = C^2, g = S^2, b = C S (C and S are the lock-in means b and a of the state),
// a = 1. Off the plate 0. A chan.from_tex with measure sum turns it into four numbers.
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let t = textureLoad(in0, vec2i(v.pos.xy), 0);
    if t.g > 15000.0 {
        return vec4f(0.0);
    }
    return vec4f(t.b * t.b, t.a * t.a, t.b * t.a, 1.0);
}
