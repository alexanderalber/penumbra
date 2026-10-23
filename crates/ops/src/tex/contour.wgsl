// tex.contour: lines where the chosen channel (straight colour) crosses the levels `offset` plus a
// multiple of `interval`, `thickness` pixels wide whatever the slope. Only levels strictly inside
// `range` get a line, so an input clipped at 0 or 1 gets no outline around its flat parts.
//
// `stage` (set by the node, not a user param) picks what a pass does:
//   0  draw, the channel read from in0;
//   1  blur the channel of in0 across (`radius`, the node's `smooth`);
//   2  blur the result of 1 (in1) down;
//   3  draw over in0, the channel read from the blurred field in in1.
// The blur weighs each pixel by its alpha, so transparent parts do not pull the values towards 0
// near their edge. The blurred field is stored as a 16-bit float plus the remainder in a second
// channel: one 16-bit float alone would put the smoothed values back onto steps.
//
// The width: the distance to the level in pixels is the distance in value divided by the slope,
// the length of the gradient from central differences over the four neighbours (per pixel, so
// the edge of a line does not step with 2 x 2 blocks as fwidth would, and a diagonal line is as
// wide as a straight one). The slope has a floor at the precision the field is stored with (half
// a step of a 16-bit float, the blurred field's two parts far less): below it a difference is
// rounding, and the division would turn it into a smear. A pixel whose neighbours all have its
// value (the inside of a flat, or of one step of a quantised map) gets no line: where the level
// lies in such a flat is not known. The floor is deliberately not the input's quantisation step
// (1/255 for an 8-bit map): that floor would flood every gentle slope with a band one step
// wide; a stepped map wants `smooth` instead. Where the value changes by an interval or more per
// pixel the lines would merge into a smear; they fade out there, which also keeps the edge of a
// transparent region free of lines.

const pn_draw = 0;
const pn_smooth_x = 1;
const pn_smooth_y = 2;
const pn_draw_smoothed = 3;

fn pn_gauss(x: f32, sigma: f32) -> f32 {
    return exp(-0.5 * x * x / (sigma * sigma));
}

// One step of a 16-bit float at x.
fn pn_step(x: f32) -> f32 {
    let e = max(floor(log2(max(abs(x), 1e-30))), -14.0);
    return exp2(e - 10.0);
}

fn pn_value(c: vec4f) -> f32 {
    switch params.channel {
        case channel_red: { return c.r; }
        case channel_green: { return c.g; }
        case channel_blue: { return c.b; }
        case channel_alpha: { return c.a; }
        default: { return pn_luma(c.rgb); }
    }
}

fn pn_load(t: texture_2d<f32>, p: vec2i) -> vec4f {
    return textureLoad(t, clamp(p, vec2i(0), vec2i(textureDimensions(t)) - 1), 0);
}

// The value the levels are tested against at pixel p.
fn pn_field(p: vec2i) -> f32 {
    if (i32(params.stage) == pn_draw_smoothed) {
        let f = pn_load(in1, p);
        return f.r + f.g;
    }
    return pn_value(pn_unpremultiply(pn_load(in0, p)));
}

// One axis of the alpha-weighted Gaussian: value (in two parts) and mean weight.
fn pn_smooth(p: vec2i, axis: vec2i, first: bool) -> vec4f {
    let radius = clamp(params.radius, 0.0, 64.0);
    let sigma = max(radius / 3.0, 0.3);
    let n = i32(ceil(radius));
    var sum = 0.0;
    var weight = 0.0;
    var total = 0.0;
    for (var i = -n; i <= n; i++) {
        let g = pn_gauss(f32(i), sigma);
        var x: f32;
        var a: f32;
        if (first) {
            let c = pn_load(in0, p + i * axis);
            x = pn_value(pn_unpremultiply(c));
            // The alpha channel's own field: transparent is a value there, not a gap.
            a = select(c.a, 1.0, params.channel == channel_alpha);
        } else {
            let f = pn_load(in1, p + i * axis);
            x = f.r + f.g;
            a = f.b;
        }
        sum += g * a * x;
        weight += g * a;
        total += g;
    }
    var m = 0.0;
    if (weight > 1e-6) {
        m = sum / weight;
    }
    // The mantissa cut to the 10 bits of a 16-bit float: stored exactly, whatever the rounding.
    let hi = bitcast<f32>(bitcast<u32>(m) & 0xffffe000u);
    return vec4f(hi, m - hi, weight / max(total, 1e-6), 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let stage = i32(params.stage);
    if (stage == pn_smooth_x) {
        return pn_smooth(p, vec2i(1, 0), true);
    }
    if (stage == pn_smooth_y) {
        return pn_smooth(p, vec2i(0, 1), false);
    }
    let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let interval = max(params.interval, 1e-6);
    let x = pn_field(p);
    let grad = vec2f(
        pn_field(p + vec2i(1, 0)) - pn_field(p - vec2i(1, 0)),
        pn_field(p + vec2i(0, 1)) - pn_field(p - vec2i(0, 1)),
    ) * 0.5;
    // Value change per pixel, in intervals.
    let slope = length(grad) / interval;
    let smoothed = stage == pn_draw_smoothed;
    let least = pn_step(x) * select(0.5, 1.0 / 2048.0, smoothed) / interval;
    let t = (x - params.offset) / interval;
    let level = round(t);
    let value = level * params.interval + params.offset;
    var line = 0.0;
    if (slope > 0.0 && value > params.range.x && value < params.range.y) {
        let d = abs(t - level) / max(slope, least);
        let half = 0.5 * params.thickness;
        line = (1.0 - smoothstep(half - 0.5, half + 0.5, d)) * (1.0 - smoothstep(0.5, 1.0, slope));
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
