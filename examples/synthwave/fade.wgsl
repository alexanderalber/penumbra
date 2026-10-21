// The render faded with distance, so the lines melt into the sky towards the horizon.
// in0: the render's picture, in1: its data output (the distance from the camera in alpha, 0
// where nothing was drawn).
// @param start float = 5 0..50 -- metres where the fade begins
// @param end float = 11 0..100 -- metres where nothing is left
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = vec2i(v.pos.xy);
    let last = vec2i(pn.resolution) - 1;
    let c = textureLoad(in0, at, 0);
    // The data pass has no antialiasing, so the soft edge of a line can fall on a pixel it
    // left empty: there the nearest distance around stands in.
    var d = textureLoad(in1, at, 0).a;
    if d <= 0.0 {
        d = 1e9;
        for (var dy = -1; dy <= 1; dy++) {
            for (var dx = -1; dx <= 1; dx++) {
                let n = textureLoad(in1, clamp(at + vec2i(dx, dy), vec2i(0), last), 0).a;
                if n > 0.0 {
                    d = min(d, n);
                }
            }
        }
    }
    return c * (1.0 - smoothstep(params.start, params.end, d));
}
