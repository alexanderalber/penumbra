// tex.contour: lines where the chosen channel (straight colour) crosses the levels `offset` plus a
// multiple of `interval`, `thickness` pixels wide whatever the slope (the distance to the level is
// divided by how much the value changes per pixel). Only levels strictly inside `range` get a
// line, so an input clipped at 0 or 1 gets no outline around its flat parts. Where the value
// changes by an interval or more per pixel the lines would merge into a smear; they fade out
// there, which also keeps the edge of a transparent region free of lines.

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let c = pn_unpremultiply(src);
    var x: f32;
    switch params.channel {
        case channel_red: { x = c.r; }
        case channel_green: { x = c.g; }
        case channel_blue: { x = c.b; }
        case channel_alpha: { x = c.a; }
        default: { x = pn_luma(c.rgb); }
    }
    let t = (x - params.offset) / max(params.interval, 1e-6);
    // Derivatives before any branch: they need all four pixels of the quad.
    let w = fwidth(t);
    let level = round(t);
    let value = level * params.interval + params.offset;
    var line = 0.0;
    if (w > 1e-6 && value > params.range.x && value < params.range.y) {
        let d = abs(t - level) / w;
        let half = 0.5 * params.thickness;
        line = (1.0 - smoothstep(half - 0.5, half + 0.5, d)) * (1.0 - smoothstep(0.5, 1.0, w));
    }
    let e = line * src.a * params.color.a;
    let ink = vec4f(params.color.rgb * e, e);
    switch params.mode {
        case mode_alpha: {
            return ink;
        }
        case mode_over: {
            return ink + src * (1.0 - e);
        }
        default: {
            return vec4f(ink.rgb, 1.0);
        }
    }
}
