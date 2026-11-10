// Numbers about the world per block of state texels, for measure.wgsl. This node is 32 x 18;
// each texel covers its share of the state. r: the share of the block that is alive (any channel
// above `mark`) where life may live, g: the share where life may live, b: the mean change of
// the state in this frame, a: the mean of the state (its mass).
//
// in0: the state after this frame (step), in1: the state before it (the tex.feedback),
// in2: fields.wgsl (b above 0.5 where the mask lets nothing live).
//
// @param mark float = 0.1 0.01..0.9 -- a texel counts as alive where a channel is above this

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let big = vec2i(textureDimensions(in0));
    let small = vec2i(32, 18);
    let lo = p * big / small;
    let hi = (p + vec2i(1)) * big / small;
    var covered = 0.0;
    var allowed = 0.0;
    var change = 0.0;
    var mass = 0.0;
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            let q = vec2i(x, y);
            let a = textureLoad(in0, q, 0).rgb;
            let b = textureLoad(in1, q, 0).rgb;
            let ok = select(1.0, 0.0, textureLoad(in2, q, 0).b > 0.5);
            allowed += ok;
            covered += ok * select(0.0, 1.0, max(max(a.r, a.g), a.b) > params.mark);
            let d = abs(a - b);
            change += d.r + d.g + d.b;
            mass += a.r + a.g + a.b;
        }
    }
    let cnt = f32(max((hi.x - lo.x) * (hi.y - lo.y), 1));
    return vec4f(covered / cnt, allowed / cnt, change / cnt, mass / cnt);
}
