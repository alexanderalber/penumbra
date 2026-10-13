// What the channels saw, drawn over the picture they came from:
// in0 the picture, in1 its brightness per column (a 128 x 1 texture made from channels),
// in2 its mean colour per column (64 x 1), in3 its mean colour (one bound constant).
// @param gain float = 4 1..10 -- how much the brightness curve is stretched around 0.5
// @param band float = 0.1 0..0.3 -- height of the colour strip at the bottom
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let uv = v.uv;
    var c = textureSampleLevel(in0, pn_clamp, uv, 0.0).rgb * 0.6;

    // The brightness curve: one channel sample per column, read back as a texture.
    let lum = textureSampleLevel(in1, pn_clamp, vec2f(uv.x, 0.5), 0.0).r;
    let y = 0.45 - (lum - 0.5) * params.gain * 0.45;
    let px = abs(uv.y - y) * pn.resolution.y;
    c = mix(c, vec3f(1.0), clamp(2.5 - px, 0.0, 1.0));

    // The colour strip: the mean colour of each of 64 columns, nearest so the steps show.
    if (uv.y > 1.0 - params.band) {
        let n = f32(textureDimensions(in2).x);
        let x = (floor(uv.x * n) + 0.5) / n;
        c = textureSampleLevel(in2, pn_clamp, vec2f(x, 0.5), 0.0).rgb;
    }

    // The mean colour of the whole picture, a square at the top right.
    let p = uv * pn.resolution;
    let side = 0.14 * pn.resolution.y;
    let margin = 0.04 * pn.resolution.y;
    if (p.x > pn.resolution.x - margin - side && p.x < pn.resolution.x - margin && p.y > margin && p.y < margin + side) {
        c = textureSampleLevel(in3, pn_clamp, vec2f(0.5), 0.0).rgb;
    }
    return vec4f(c, 1.0);
}
