// One step of a feedback loop that remembers, per pixel, when the person was last there. in0 is
// the mask (white where the person is), in1 this picture one frame earlier. Where the person is
// now, the time is written; everywhere else the old value stays, so the latest visit wins.
// The time is kept in two parts, red the whole 16-second steps and green the share of the
// current step. One value alone would lose precision after a few minutes in a 16-bit loop.
// Blue is 1 where the person has been at all.
// @param threshold float = 0.5 0..1 -- mask value from which a pixel counts as the person
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let mask = textureSampleLevel(in0, pn_clamp, v.uv, 0.0).r;
    if mask < params.threshold {
        return textureSampleLevel(in1, pn_clamp, v.uv, 0.0);
    }
    let t = pn_time.seconds / 16.0;
    return vec4f(floor(t), fract(t), 1.0, 1.0);
}
