// The valley's night sky before sunrise: a dark gradient from night blue at the horizon to
// obsidian overhead, and the first light as a band lying on the horizon. `dawn` sets the band:
// at 0 a thin dark red line, at 1 a broad copper band with a gold core and a faint halo above
// it. `reveal` widens the band beyond that for one moment. `sun` lifts the top of a sun disc out of
// the band, copper low down and gold towards 1; the disc stays gold and never turns white. The swell (the stage's slow signal, high in a breakdown) lifts the
// band and makes it glow; the bass pulses its core a little. Black below the horizon, where the
// lake lies (lake.wgsl). The horizon is where the floor fades out, `reach` metres ahead, as the
// camera sees it, so it moves with the camera.
// @param dawn float = 0 0..1 -- the first light: 0 a thin dark red line, 1 a broad copper band with a gold core
// @param reveal float = 0 0..1 -- a moment of the widest light: towards 1 the band grows wider and taller than at dawn 1. The stage also lifts `dawn` to at least this value
// @param sun float = 0 0..1 -- how far the sun has risen: 0 under the horizon, 1 the disc's top stands half of `sun_size` above the horizon
// @param sun_size float = 0.16 0.02..0.6 -- the sun disc's radius, in picture heights
// @param swell float = 0 0..1 -- the stage's swell: lifts the band and adds to its glow
// @param energy float = 0 0..1 -- the bass, curved: pulses the band's core a little
// @param warmth float = 0 0..1 -- 0 the ember palette (amber halo), 1 the jade palette (cold blue-green halo)
// @param dim float = 0.7 0..1 -- the sky's light: 1 full, 0 half
// @param band_height float = 0.1 0.01..0.5 -- how far the band reaches up at dawn 1, in picture heights
// @param band_width float = 0.9 0.1..4 -- how far the band reaches to either side at dawn 1, in picture heights
// @param eye float = 1.2 0..10 -- the camera's height over the floor, metres (height of the stage's pose)
// @param pitch float = -1 -60..60 -- the camera's tilt up, degrees
// @param fov float = 58 10..120 -- the camera's vertical field of view, degrees
// @param roll float = 0 -45..45 -- the camera's roll, degrees
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees
// @param reach float = 40 1..200 -- metres ahead where the floor fades out: the horizon lies there

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. `.z`: one pixel in the same unit. The same helper as the sky elements'.
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

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let x = at.x;
    let y = at.y;
    let px = max(at.z, 1e-5);
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let jade = clamp(params.warmth, 0.0, 1.0);
    let light = 0.5 + 0.5 * clamp(params.dim, 0.0, 1.0);

    // Above the horizon: night blue low down, obsidian overhead.
    let up = max(y, 0.0);
    let low = mix(vec3f(0.0060, 0.0085, 0.0230), vec3f(0.0035, 0.0105, 0.0190), jade);
    let high = vec3f(0.0012, 0.0016, 0.0042);
    var sky = mix(low, high, smoothstep(0.0, 0.7, up)) * (0.75 + 0.25 * light);

    // The band: its height and width grow with the dawn, the swell lifts it further.
    let reveal = smoothstep(0.0, 1.0, clamp(params.reveal, 0.0, 1.0));
    let h = max(params.band_height, 0.001) * mix(0.07, 1.0, dawn) * (1.0 + 0.7 * swell) * (1.0 + 0.15 * reveal);
    let w = max(params.band_width, 0.01) * mix(0.4, 1.0, dawn) * (1.0 + 0.25 * swell) * (1.0 + 0.9 * reveal);
    let across = exp(-(x * x) / (w * w));
    // Thin bright core, body, and a wide faint halo above.
    let core_h = max(0.22 * h, 1.2 * px);
    let core = exp(-up / core_h);
    let body = exp(-up / h);
    let halo = exp(-up / (2.5 * h + 0.015)) * mix(0.25, 1.0, dawn);
    let core_c = mix(vec3f(0.05, 0.004, 0.002), vec3f(0.75, 0.40, 0.12), dawn);
    let body_c = mix(vec3f(0.012, 0.001, 0.0006), vec3f(0.10, 0.030, 0.008), dawn);
    let halo_c = mix(vec3f(0.008, 0.003, 0.001), vec3f(0.0015, 0.006, 0.007), jade) * (0.2 + 0.8 * dawn);
    let glow = (1.0 + 0.9 * swell) * light;
    let pulse = 1.0 + 0.25 * clamp(params.energy, 0.0, 1.0);
    // The core narrows towards the sides faster than the body: the light gathers in the middle.
    let band = core_c * core * across * across * pulse + body_c * body * across + halo_c * halo * sqrt(across);
    sky += band * glow;

    // The sun: a disc rising behind the band's core, copper at the horizon and gold towards its top.
    // Its colours stay below white at any height.
    let sun = clamp(params.sun, 0.0, 1.0);
    if sun > 0.0 {
        let radius = max(params.sun_size, 0.001);
        let top = radius * mix(-0.02, 0.5, sun);
        let from_rim = length(vec2f(x, y - (top - radius))) - radius;
        let copper = vec3f(0.60, 0.20, 0.04);
        let gold = mix(vec3f(0.72, 0.30, 0.06), vec3f(0.88, 0.47, 0.10), sun);
        let colour = mix(copper, gold, smoothstep(-0.2 * radius, top, y));
        // Faint light round the disc, then the disc with a soft edge.
        sky += gold * 0.18 * exp(-max(from_rim, 0.0) / (0.3 * radius)) * smoothstep(0.0, 0.2, sun);
        sky = mix(sky, colour, smoothstep(0.008, -0.004, from_rim));
    }

    // Below the horizon nothing: the lake goes there. One pixel of soft edge.
    let above = smoothstep(-px, px, y);
    return vec4f(sky * above, 1.0);
}
