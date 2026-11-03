// The light of a fog bank: drifting noise inside a soft oval, for the bank's hatch lines to
// sample. Premultiplied with a small alpha, so a bank adds light and barely hides what lies
// behind; that is why the banks need no sorting.
// @param drift float = 0 0..20000 -- moves the noise; bind it to travelled
// @param detail float = 3 0.5..12 -- noise cells across a bank
// @param thickness float = 0.8 0..1 -- how much of the oval the fog fills
// @param cover float = 0.04 0..0.5 -- how much a dense spot hides what lies behind; under 0.05 the fog never takes the place of the lines behind it in the stage's distance fade
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = (v.uv - 0.5) * 2.0;
    let oval = 1.0 - smoothstep(0.35, 1.0, length(p));
    let t = params.drift * 0.05;
    let q = vec3f(v.uv.x * params.detail, v.uv.y * params.detail * 0.5, t);
    var n = 0.5 * pn_noise(q, 3u) + 0.25 * pn_noise(q * 2.03 + vec3f(5.1, 1.7, 0.4 * t), 4u)
        + 0.125 * pn_noise(q * 4.1, 5u);
    n = clamp(0.5 + n, 0.0, 1.0);
    let d = clamp((n - (1.0 - params.thickness)) / max(params.thickness, 0.05), 0.0, 1.0) * oval;
    return vec4f(vec3f(d), params.cover * d);
}
