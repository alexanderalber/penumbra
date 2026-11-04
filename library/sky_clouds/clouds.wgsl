// A deck of clouds low over the horizon, over black, to screen over the sky of `stage_valley`.
// The band below lights the clouds from underneath: their lower edges glow copper, most in the
// middle over the band and most at dawn and in the swell. Above, the clouds stay dark. The sky
// is screened, so the clouds cannot hide the stars or the band. The deck drifts slowly towards
// the camera as the world travels, and a little sideways. Nothing below the horizon.
// @param presence float = 1 0..1 -- how much of the deck is there: 1 all of it, 0 nothing (it thins out towards 0)
// @param brightness float = 1 0..3 -- the light of everything
// @param cover float = 0.55 0..1 -- how much of the deck is cloud: 0 a few wisps, 1 nearly closed
// @param low float = 0.025 0..0.3 -- where the deck begins above the horizon, in picture heights
// @param top float = 0.22 0.02..0.8 -- where the deck ends above the horizon, in picture heights
// @param size float = 1 0.2..5 -- the clouds' size
// @param spread float = 0.8 0.1..4 -- how far to the sides the band lights the clouds, in picture heights
// @param under float = 1 0..3 -- the light on the clouds' undersides
// @param drift float = 0.004 0..0.05 -- how fast the deck comes towards the camera, per beat travelled
// @param wind float = 0.001 -0.02..0.02 -- how fast the deck moves sideways, per beat travelled
// @param warmth float = 0 0..1 -- 0 the ember palette (copper), 1 the jade palette (pale bronze edges, jade shade)
// @param seed int = 0 0..1000 -- another deck
// @param dawn float = 0 0..1 -- the stage's dawn (bind it to dawn of the stage's pose): more light from below towards 1
// @param swell float = 0 0..1 -- the stage's swell (swell of the stage's pose): the band's glow on the clouds
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the drift's clock
// @param eye float = 1.2 0..10 -- the camera's height over the floor, metres (height of the stage's pose)
// @param pitch float = -1 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 58 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 40 1..200 -- metres ahead where the floor fades out, the stage's `end` (far of the pose): the horizon lies there

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. `.z`: one pixel in the same unit. The same helper as the stage's sky.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan(down)) / unit, 2.0 * t / (res.y * unit));
}

fn fbm(p: vec3f, seed: u32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    for (var i = 0u; i < 5u; i++) {
        sum += amp * pn_noise(q, seed + i * 13u);
        q = q * vec3f(2.1, 2.1, 1.7) + vec3f(4.3, 1.9, 7.7);
        amp *= 0.5;
    }
    return sum;
}

// Cloud density at sky place (x, y): the deck is a flat layer seen from below, so its clouds
// crowd together towards the horizon. 0 clear sky, 1 thick cloud.
fn density(x: f32, y: f32) -> f32 {
    let lo = max(params.low, 0.0);
    let hi = max(params.top, lo + 0.01);
    let in_deck = smoothstep(lo * 0.5, lo + 0.02, y) * (1.0 - smoothstep(0.5 * (lo + hi), hi, y));
    if in_deck <= 0.0 {
        return 0.0;
    }
    // Distance into the layer: 1/y, as a flat ceiling far overhead.
    let far = pow(1.0 / max(y, 0.008), 0.7);
    let k = 0.9 / max(params.size, 0.05);
    let s = f32(max(params.seed, 0));
    let p = vec3f((x * far + params.beats * params.wind * 40.0) * k * 0.9,
                  (far + params.beats * params.drift * 40.0) * k * 0.9, s * 0.61);
    // Banks and clear stretches at a larger scale.
    let bank = pn_noise(vec3f(p.x * 0.12, p.y * 0.2, s * 0.29 + 3.0), 67u);
    let n = fbm(p, 61u) + 0.35 * bank;
    let cover = clamp(params.cover, 0.0, 1.0) * mix(0.35, 1.0, clamp(params.presence, 0.0, 1.0));
    let c = smoothstep(0.35 - 0.7 * cover, 0.6 - 0.7 * cover, n);
    return c * in_deck;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let x = at.x;
    let y = at.y;
    let px = max(at.z, 1e-5);
    if y < -2.0 * px {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let jade = clamp(params.warmth, 0.0, 1.0);

    let d = density(x, y);
    if d <= 0.0 {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    // The underside: cloud here but less of it just below, so the light from the band reaches it.
    let dy = max(0.006, 3.0 * px);
    let below = density(x, y - dy);
    let edge_lit = clamp(d - 0.85 * below, 0.0, 1.0);
    // The band's light: in the middle, low down, more at dawn and in the swell.
    let w = max(params.spread, 0.05) * mix(0.55, 1.0, dawn);
    let across = exp(-(x * x) / (w * w));
    let near_band = exp(-y / (0.06 + 0.05 * dawn));
    let lamp = across * near_band * mix(0.25, 1.0, dawn) * (0.6 + 0.8 * swell);
    let copper = mix(vec3f(0.55, 0.24, 0.065), vec3f(0.36, 0.27, 0.14), jade);
    let gold = vec3f(0.80, 0.45, 0.12);
    let under_c = mix(copper, gold, 0.4 * dawn * across);
    var light = under_c * edge_lit * lamp * 0.7 * max(params.under, 0.0);
    // The cloud's body, a faint glow through it from the band, dark above.
    light += copper * d * lamp * 0.06;
    // A trace of the night on the tops, jade in the jade palette.
    light += mix(vec3f(0.004, 0.005, 0.010), vec3f(0.002, 0.008, 0.007), jade) * d * (1.0 - near_band);

    let edge = smoothstep(-px, px, y);
    let out = light * max(params.brightness, 0.0) * clamp(params.presence, 0.0, 1.0) * edge;
    return vec4f(out, 1.0);
}
