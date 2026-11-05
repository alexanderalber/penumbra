// A volcano far off to one side of the horizon, over black, for the sky of `stage_valley`. The
// picture is light plus an alpha: the mountain and its plume cover what they stand in front of
// when a tex.composite lays them `over` the stars, and the light adds on top. The band stays lit
// behind it, since the stage screens the sky onto the band. Its outline carries a thin rim of the
// dawn's light on the side towards the band, and its flank towards the band a faint sheen. Its
// crater glows like embers. A drop makes the crater flare: the glow, the lava
// lines down the flank and the underside of the smoke plume brighten and sink back over a few
// seconds. The plume rises from the crater and leans with the wind, dark above, lit from below.
// The bass makes the embers flicker a little. Nothing below the horizon.
// @param presence float = 1 0..1 -- how much of the volcano is there: 1 all of it, 0 nothing
// @param brightness float = 1 0..3 -- the light of everything
// @param place float = 0.62 -1.5..1.5 -- where the volcano stands from straight ahead, in picture heights (left negative)
// @param tall float = 0.09 0.01..0.4 -- the volcano's height over the horizon, in picture heights
// @param wide float = 3.2 1..8 -- the foot's half width, in heights of the volcano
// @param crater float = 1 0..3 -- the embers' light in the crater between drops
// @param flare float = 0 0..1 -- the drop's flare, 1 at the drop and sinking (the component binds it from the bus)
// @param lava float = 1 0..3 -- the light of the lava lines on a flare; 0 none
// @param plume float = 1 0..3 -- the smoke plume's light; 0 no plume
// @param rise float = 0.3 0.05..1 -- how high the plume rises, in picture heights
// @param wind float = 0.6 -3..3 -- how far the plume leans, away from the middle at positive values
// @param rim float = 1 0..3 -- the light on the outline
// @param warmth float = 0 0..1 -- 0 the ember palette, 1 the jade palette (the rim turns jade, the embers stay)
// @param seed int = 0 0..1000 -- another outline and another plume
// @param dawn float = 0 0..1 -- the stage's dawn (bind it to dawn of the stage's pose): brighter rim and sheen towards 1
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): a small flicker of the embers
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the plume's clock
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
        sum += amp * pn_noise(q, seed + i * 17u);
        q = q * 2.03 + vec3f(1.7, 9.2, 3.1);
        amp *= 0.5;
    }
    return sum;
}

// The outline's height over the horizon at dx from the volcano's middle, in picture heights.
fn ridge(dx: f32, h: f32, hw: f32, cw: f32, s: f32) -> f32 {
    let a = abs(dx);
    // Concave flanks rising to a cut-off top.
    var top = h * pow(max(1.0 - a / hw, 0.0), 2.2) / pow(1.0 - cw / hw, 2.2);
    top = min(top, h);
    // The crater: a shallow notch in the top.
    let dip = 0.05 * h * max(1.0 - (dx / cw) * (dx / cw), 0.0);
    top -= dip;
    // A rough outline: shoulders and gullies, more of them low down.
    let rough = pn_noise(vec3f(dx * 9.0 / hw, s * 0.53, 1.0), 41u) + 0.35 * pn_noise(vec3f(dx * 23.0 / hw, s * 0.53, 2.0), 41u);
    top += h * 0.035 * rough * smoothstep(0.0, 0.3 * hw, a);
    return top;
}

fn below_of(dx: f32, y: f32, h: f32, hw: f32, cw: f32, s: f32) -> f32 {
    return ridge(dx, h, hw, cw, s) - y;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let y = at.y;
    let px = max(at.z, 1e-5);
    if y < -2.0 * px {
        return vec4f(0.0);
    }
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let jade = clamp(params.warmth, 0.0, 1.0);
    let flare = clamp(params.flare, 0.0, 1.0);
    let s = f32(max(params.seed, 0));
    let h = max(params.tall, 0.005);
    let hw = h * max(params.wide, 1.0);
    let cw = 0.09 * hw;
    let dx = at.x - params.place;
    // +1 on the flank that faces the band in the middle of the view, -1 on the far flank.
    let toward = -sign(params.place + 1e-4);
    let inner = smoothstep(-0.3 * hw, 0.3 * hw, dx * toward);

    var light = vec3f(0.0);
    // How much of what lies behind the mountain and the plume hide.
    var hides = smoothstep(-px, px, below_of(dx, y, h, hw, cw, s));
    let top = ridge(dx, h, hw, cw, s);
    let below = top - y;
    let summit = h * 0.95;

    // The rim: a thin line of the dawn's light along the outline, brighter on the band side.
    let rim_c = mix(vec3f(0.55, 0.22, 0.06), vec3f(0.10, 0.40, 0.26), jade);
    let line = exp(-abs(below) / (1.1 * px)) * smoothstep(hw, 0.6 * hw, abs(dx));
    let rim_level = 0.22 * mix(0.35, 1.0, dawn) * mix(0.25, 1.0, inner) * max(params.rim, 0.0);
    light += rim_c * line * rim_level;

    // The body: a faint sheen on the band side, fading down the flank.
    if below > 0.0 && y >= 0.0 {
        let sheen = exp(-below / (0.25 * h)) * inner * 0.03 * mix(0.4, 1.0, dawn);
        light += mix(vec3f(0.35, 0.16, 0.06), vec3f(0.10, 0.25, 0.18), jade) * sheen;
    }

    // The embers: the crater's glow, a small flicker with the bass, flaring on a drop.
    let ember_c = vec3f(0.95, 0.30, 0.05);
    let flicker = 1.0 + 0.15 * clamp(params.bass, 0.0, 1.0) * pn_noise(vec3f(params.beats * 2.0, s, 7.0), 43u);
    let ember = (0.3 * max(params.crater, 0.0) + 1.2 * flare) * flicker;
    // From below the crater's inside is hidden: the glow shows on its lip and rises above it.
    let over = max(-below, 0.0);
    let crater = exp(-(dx * dx) / (cw * cw)) * smoothstep(-1.5 * px, 0.5 * px, -below)
               * (exp(-over / (0.07 * h)) + 0.8 * exp(-over / (1.2 * px)));
    let ch = vec2f(dx / (2.2 * cw), (y - summit) / (0.9 * h));
    let haze = exp(-dot(ch, ch));
    light += ember_c * (crater * 0.9 + haze * 0.08) * ember;

    // The lava lines: a few threads down the band-side flank, lit by the flare.
    if below > 0.0 && flare > 0.001 {
        let down = clamp((summit - y) / summit, 0.0, 1.0);
        let wob = 0.04 * hw * pn_noise(vec3f(y * 60.0 / h, s, 3.0), 47u);
        var threads = 0.0;
        for (var k = 0; k < 3; k++) {
            let fk = f32(k);
            let lean = (0.18 + 0.22 * fk) * toward * (1.0 - 0.6 * fk / 2.0);
            let cx = lean * (summit - y) * hw / summit * 0.9 + wob;
            let wdt = (0.8 + 1.2 * down) * px + 0.004 * h;
            let len = 0.35 + 0.25 * fk;
            threads += exp(-abs(dx - cx) / wdt) * exp(-down / (len * mix(0.4, 1.0, flare)));
        }
        light += ember_c * threads * flare * 0.35 * max(params.lava, 0.0);
    }

    // The plume: smoke rising from the crater and leaning with the wind, lit from below.
    if params.plume > 0.0 {
        let up = y - summit;
        if up > -0.2 * h {
            let rise = max(params.rise, 0.02);
            let u = max(up, 0.0);
            let lean = -toward * params.wind * pow(u / rise, 1.4) * rise * 0.5;
            let wdt = cw * 0.9 + u * 0.35;
            let across = (dx - lean) / wdt;
            let clock = params.beats * 0.012;
            let n = fbm(vec3f(across * 1.2, (u - clock * 0.05) * 14.0 / rise, clock * 0.3 + s), 53u);
            var dens = exp(-across * across) * smoothstep(-0.2 * h, 0.0, up) * exp(-u / rise);
            dens *= clamp(0.55 + 0.9 * n, 0.0, 1.5);
            // Lit from below by the crater, more on a flare; dark above, a trace of night blue.
            let underlit = exp(-u / (0.06 + 0.06 * flare)) * (0.10 + 0.9 * ember);
            let col = ember_c * 0.30 * underlit + vec3f(0.016, 0.019, 0.030) * (0.4 + 0.6 * dawn);
            light += col * dens * max(params.plume, 0.0);
            hides = max(hides, clamp(dens * max(params.plume, 0.0), 0.0, 0.85));
        }
    }

    let edge = smoothstep(-px, px, y);
    let there = clamp(params.presence, 0.0, 1.0) * edge;
    // A mountain at half presence (a breakdown dims the sky) still stands whole in front of the stars.
    return vec4f(light * max(params.brightness, 0.0) * there, hides * smoothstep(0.0, 0.35, there));
}
