// What the channels saw, drawn over the picture they came from:
// in0 the picture, in1 its brightness per column (a 128 x 1 texture made from channels),
// in2 its mean colour per column (64 x 1), in3 its mean colour (one bound constant).
// @param gain float = 5 1..10 -- how much the brightness curve is stretched around the mean
// @param band float = 0.1 0..0.3 -- height of the colour strip at the bottom
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.uv;
    let px = pn.resolution.y; // pixels per image height
    let mean = textureSampleLevel(in3, pn_clamp, vec2f(0.5), 0.0).rgb;
    var c = textureSampleLevel(in0, pn_clamp, uv, 0.0).rgb * 0.7;

    // The brightness curve: one channel sample per column, read back as a texture. It is drawn
    // around the mean brightness, so it stays in view whatever the picture's colours.
    let lum = textureSampleLevel(in1, pn_clamp, vec2f(uv.x, 0.5), 0.0).r;
    let y = 0.45 - (lum - pn_luma(mean)) * params.gain * 0.45;
    let d = (uv.y - y) * px;
    // A faint fill below the line, a soft glow around it, and the line itself.
    if (d > 0.0 && uv.y < 1.0 - params.band) {
        c = mix(c, vec3f(1.0), 0.08 * exp(-d / (0.25 * px)));
    }
    c += vec3f(0.25) * exp(-abs(d) / 6.0);
    c = mix(c, vec3f(1.0), clamp(2.0 - abs(d), 0.0, 1.0));

    // The colour strip: the mean colour of each of 64 columns, nearest so the steps show,
    // under a thin dark seam.
    let top = 1.0 - params.band;
    if (uv.y > top) {
        let n = f32(textureDimensions(in2).x);
        let x = (floor(uv.x * n) + 0.5) / n;
        c = textureSampleLevel(in2, pn_clamp, vec2f(x, 0.5), 0.0).rgb;
        c *= clamp((uv.y - top) * px / 3.0, 0.0, 1.0);
    }

    // The mean colour of the whole picture, a square with a white rim at the top right.
    let p = uv * pn.resolution;
    let side = 0.14 * px;
    let margin = 0.04 * px;
    let centre = vec2f(pn.resolution.x - margin - side * 0.5, margin + side * 0.5);
    let q = abs(p - centre) - vec2f(side * 0.5);
    let box = max(q.x, q.y); // < 0 inside the square, in pixels
    c = mix(c, c * 0.4, clamp(1.0 - (box - 4.0) / 12.0, 0.0, 1.0) * 0.6); // a soft shadow
    c = mix(c, vec3f(1.0), clamp(1.5 - abs(box), 0.0, 1.0)); // the rim
    if (box < -1.0) {
        c = mean;
    }
    return vec4f(c, 1.0);
}
