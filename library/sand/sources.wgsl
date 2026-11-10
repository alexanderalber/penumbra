// Where new sand comes from, at the simulation size: the step pours grains where this picture
// is bright. Its colour is the grains' colour with colors source. It holds the component's
// source input (tracked people, moving dots) and a rain line along the top.
//
// in0: the source input. Its colours are kept.
//
// @param blobs float = 0.0 0..1 -- how strongly the source input pours
// @param rain float = 1.0 0..1 -- a line along the top that pours a steady rain of grains
// @param rain_from float = 0.0 0..1 -- left end of the rain line, in picture widths
// @param rain_to float = 1.0 0..1 -- right end of the rain line
// @param rain_color color = [1, 0.75, 0.4, 1] -- colour of the rain, for colors source

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    var c = textureSampleLevel(in0, pn_clamp, v.uv, 0.0).rgb * params.blobs;
    let row = v.pos.y < 1.0;
    if row && params.rain > 0.0 && v.uv.x >= params.rain_from && v.uv.x <= params.rain_to {
        let rc = params.rain_color.rgb;
        c = max(c, rc / max(max(rc.r, max(rc.g, rc.b)), 1e-4) * params.rain);
    }
    return vec4f(c, 1.0);
}
