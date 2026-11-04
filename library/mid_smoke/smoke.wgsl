// The smoke of mid_smoke: three veils of copal smoke, one above the other, for the veils' models
// to show (veil1.obj to veil3.obj take a third each, the top one first). Each veil is thick near
// its foot and frays into wisps towards the top and the sides. The wisps curl slowly and drift
// with the beats travelled. The smoke is lit from behind by the band on the horizon: its thin
// edges and the tops of its puffs glow in the band's colour, the thick parts less.
// The picture is premultiplied light with almost no alpha (0.3 % where there is smoke): the smoke
// adds light and hides nothing, so it needs no sorting, and the stage's render leaves it out of
// its distances. Its alpha does not fade with the veils' light, so it stays that small. The swell thickens it: more of the picture holds smoke in a quiet stretch.
// Render it 512 x 384.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the wisps curl and drift with it
// @param swell float = 0 0..1 -- the stage's swell: thickens the smoke
// @param dawn float = 0 0..1 -- the stage's dawn: 0 a dark ember red, 1 copper
// @param warmth float = 0 0..1 -- 0 ember and copper, 1 cooler, towards jade grey
// @param cover float = 0.3 0..1 -- how much of a veil holds smoke in a loud stretch
// @param cover_swell float = 0.3 0..1 -- how much more the full swell covers
// @param curl float = 1 0..3 -- how much the wisps curl
// @param speed float = 1 0..4 -- how fast the wisps change
// @param seed int = 0 0..1000 -- other smoke

fn fbm(p: vec3f, seed: u32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    for (var o = 0; o < 5; o++) {
        sum += amp * pn_noise(q, seed + u32(o) * 17u);
        q = q * 2.03 + vec3f(1.7, 9.2, 0.0);
        amp *= 0.5;
    }
    return sum;
}

// The smoke's density at (x, y) of veil `row`, x 0..1 across, y 0..1 down from its top.
fn density(x: f32, y: f32, row: u32) -> f32 {
    let seed = u32(max(params.seed, 0)) * 3u + row;
    let t = params.beats * 0.015 * params.speed;
    // Four times as wide as high, as the veils are; the wisps drift to the side and rise.
    let p = vec3f(x * 4.0 + 0.05 * params.beats * params.speed, y * 1.4 + 0.02 * params.beats * params.speed, t);
    let warp = vec3f(fbm(p * 0.6 + vec3f(3.1, 0.0, 0.3 * t), seed + 101u), fbm(p * 0.6 + vec3f(0.0, 5.7, 0.2 * t), seed + 211u), 0.0);
    let q = p + params.curl * warp;
    let body = fbm(q, seed) * 0.5 + 0.5;
    // Strands: thin ridges of a second noise, stretched along the drift, where the smoke is thin.
    let ridge = 1.0 - abs(fbm(q * vec3f(1.6, 3.2, 1.0) + vec3f(7.3, 1.1, 0.0), seed + 307u));
    let strands = pow(clamp(ridge, 0.0, 1.0), 6.0);
    // Thick in the lower middle, fraying upwards; soft at the sides and at the very foot.
    let foot = smoothstep(1.0, 0.82, y);
    let up = smoothstep(0.0, 0.7, y);
    let sides = smoothstep(0.0, 0.22, x) * smoothstep(1.0, 0.78, x);
    let swell = clamp(params.swell, 0.0, 1.0);
    let cover = clamp(params.cover + params.cover_swell * swell, 0.0, 1.0) * mix(0.8, 1.0, up);
    let thick = smoothstep(1.0 - cover, 1.0 - cover + 0.4, body);
    let thin = strands * smoothstep(1.0 - cover - 0.2, 1.0 - cover + 0.15, body);
    return clamp(thick * mix(0.6, 1.0, up) + 0.7 * thin, 0.0, 1.0) * foot * sides;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let res = pn.resolution;
    let rows = 3.0;
    let fy = v.pos.y / res.y * rows;
    let row = u32(clamp(floor(fy), 0.0, 2.0));
    let y = fy - f32(row);
    let x = v.pos.x / res.x;
    let d = density(x, y, row);
    // The light from behind: thin smoke glows, thick smoke less; the upper edges of the puffs,
    // towards the sky, catch the most.
    let above = density(x, max(y - 0.025, 0.0), row);
    let edge = clamp((d - above) * 6.0, 0.0, 1.0);
    let glow = d * (1.0 - 0.55 * d) + 0.8 * edge * d;
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let warm = clamp(params.warmth, 0.0, 1.0);
    var tone = mix(vec3f(0.30, 0.05, 0.018), vec3f(0.55, 0.26, 0.075), dawn);
    tone = mix(tone, vec3f(0.13, 0.22, 0.19), 0.6 * warm);
    // Light, no cover: the render drops a fragment under 0.2 % alpha, so where there is smoke the
    // alpha is just above that.
    return vec4f(tone * glow, select(0.0, 0.003, d > 0.002));
}
