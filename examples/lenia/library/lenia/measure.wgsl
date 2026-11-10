// The world in four numbers, one pixel: r coverage (the share of the area where life may live
// that is alive, 0..1), g activity (how much the state changed in this frame: 100 x the mean
// change per texel), b mass (the mean of the state), a the share of the area where life may live.
// guard.wgsl reads it a frame later through a tex.feedback; the outputs read it back as channels.
//
// in0: blocks.wgsl (32 x 18).

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    var s = vec4f(0.0);
    for (var y = 0; y < 18; y++) {
        for (var x = 0; x < 32; x++) {
            s += textureLoad(in0, vec2i(x, y), 0);
        }
    }
    s /= 576.0;
    return vec4f(s.r / max(s.g, 1e-6), 100.0 * s.b, s.a, s.g);
}
