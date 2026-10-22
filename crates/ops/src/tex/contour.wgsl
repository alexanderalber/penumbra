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
// The width: the distance to the level in pixels is the distance in value divided by the slope.
// A thin line (under 3 px) takes the value from the pixel itself and the slope from central
// differences over the four neighbours (per pixel, so the edge of a line does not step with
// 2 x 2 blocks as fwidth would, and a diagonal line is as wide as a straight one). A thicker line
// takes both from a plane fitted to the field around the pixel, reaching as far to each side as
// the line is wide. On a quantised map (one step of an 8-bit field spans several pixels of a
// gentle slope, its borders ragged with the noise of a measurement) the four neighbours of a
// pixel inside a step have its value, and the pixel's own value jumps a whole step at a ragged
// border: a per-pixel slope of 0 would punch holes into a thick line and the jumps would fray its
// edges, while the fit averages the steps out. For a field that changes linearly the plane is
// the field, so the line is as wide and where it was. The slope has a floor at the precision the
// field is stored with (half a step of a 16-bit float over two pixels, the blurred field's two
// parts far less): below it a difference is rounding, and the division would turn it into a
// smear. A pixel whose surroundings all have its value (the inside of a flat, or of one step of a
// quantised map wider than the fit reaches) gets no line: where the level lies in such a flat is
// not known. The floor is deliberately not the input's quantisation step (1/255 for an 8-bit
// map): that floor would flood every gentle slope with a band one step wide; a map with steps
// wider than the line wants `smooth` instead. Where the value changes by an interval or more per
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

// A plane fitted (least squares) to the field on a grid around p reaching `reach` pixels to
// each side: up to 9 x 9 points, a pixel apart where that covers the reach, spread out where it
// does not (a cap on the cost). Grid points past the border stop there and count where they
// land, so the fit stays exact for a linear field up to the border. Returns the plane's value at p and its slope per
// pixel along x and y.
fn pn_plane(p: vec2i, reach: i32, last: vec2i) -> vec3f {
    let n = min(reach, 4);
    let stride = f32(reach) / f32(n);
    // Weighted sums of the offsets' products (1, x, y, xx, xy, yy) and of the values times 1, x, y.
    var m = mat3x3f(vec3f(0.0), vec3f(0.0), vec3f(0.0));
    var r = vec3f(0.0);
    for (var j = -n; j <= n; j++) {
        for (var i = -n; i <= n; i++) {
            let k = vec2i(round(vec2f(f32(i), f32(j)) * stride));
            let q = clamp(p + k, vec2i(0), last);
            // Weighed by alpha as the blur is: transparent parts must not pull the plane down.
            let w = select(pn_load(in0, q).a, 1.0, params.channel == channel_alpha);
            let o = vec3f(1.0, vec2f(q - p));
            m += mat3x3f(o * o.x, o * o.y, o * o.z) * w;
            r += o * (pn_field(q) * w);
        }
    }
    // A grid squeezed onto one line (an image one pixel high or wide) has no slope across it.
    let lone = vec2f(select(0.0, 1.0, m[1][1] < 0.5), select(0.0, 1.0, m[2][2] < 0.5));
    m[1][1] += lone.x;
    m[2][2] += lone.y;
    let det = determinant(m);
    if (abs(det) < 1e-6) {
        return vec3f(pn_field(p), 0.0, 0.0);
    }
    return inverse3(m, det) * r;
}

// The inverse of a 3 x 3 matrix whose determinant is `det` (WGSL has no inverse built in).
fn inverse3(m: mat3x3f, det: f32) -> mat3x3f {
    let a = m[0];
    let b = m[1];
    let c = m[2];
    return transpose(mat3x3f(cross(b, c), cross(c, a), cross(a, b))) * (1.0 / det);
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
    // Half the line's width in whole pixels, at least 1; a line wider than 16 px is not on offer.
    let reach = i32(clamp(round(0.5 * params.thickness), 1.0, 8.0));
    var x = pn_field(p);
    var grad: vec2f;
    if (reach == 1) {
        grad = vec2f(
            pn_field(p + vec2i(1, 0)) - pn_field(p - vec2i(1, 0)),
            pn_field(p + vec2i(0, 1)) - pn_field(p - vec2i(0, 1)),
        ) * 0.5;
    } else {
        let fit = pn_plane(p, 2 * reach, vec2i(textureDimensions(in0)) - 1);
        x = fit.x;
        grad = fit.yz;
    }
    // Value change per pixel, in intervals.
    let slope = length(grad) / interval;
    let smoothed = stage == pn_draw_smoothed;
    let least = pn_step(x) * select(1.0, 1.0 / 1024.0, smoothed) / f32(2 * reach) / interval;
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
