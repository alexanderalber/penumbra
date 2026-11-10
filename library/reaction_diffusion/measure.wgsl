// What the guards and the outputs measure, at a quarter of the simulation size: each texel
// sums a block of state texels. r: how much V changed in this frame (100 x the mean change),
// g: the share of the block with pattern (V above 0.1) where the coverage counts, b: the share
// where it counts (everywhere, or only where the mask is bright). The means of r and of g over
// b give the outputs activity and coverage.
//
// in0: only its size counts (a quarter of the world). in1: the state after this frame.
// in2: the state before it (the tex.feedback). in3: the object mask, any size.
//
// @param coverage_in choice(all|mask) = all -- where the coverage output counts: all of the picture, or only where the mask is bright (for a pattern that lives on the object only)

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = vec2i(textureDimensions(in1));
    let o = vec2i(textureDimensions(in0));
    let b = max(n / max(o, vec2i(1)), vec2i(1));
    let p0 = vec2i(v.pos.xy) * b;
    let inside = params.coverage_in == coverage_in_mask;
    var change = 0.0;
    var covered = 0.0;
    var allowed = 0.0;
    for (var y = 0; y < b.y; y++) {
        for (var x = 0; x < b.x; x++) {
            let q = min(p0 + vec2i(x, y), n - vec2i(1));
            let a = textureLoad(in1, q, 0).g;
            let c = textureLoad(in2, q, 0).g;
            change += abs(a - c);
            var ok = 1.0;
            if inside {
                let uv = (vec2f(q) + 0.5) / vec2f(n);
                ok = select(0.0, 1.0, pn_luma(textureSampleLevel(in3, pn_clamp, uv, 0.0).rgb) > 0.5);
            }
            allowed += ok;
            covered += ok * select(0.0, 1.0, a > 0.1);
        }
    }
    let cnt = f32(b.x * b.y);
    return vec4f(100.0 * change / cnt, covered / cnt, allowed / cnt, 1.0);
}
