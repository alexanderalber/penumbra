// The synthwave sun over black: a big disc standing on the horizon, dark horizontal lines cut
// into its lower half (wider towards the bottom, drifting slowly down), and a glow around it.
// Its colour runs from top to bottom through a warm gradient (yellow, orange, magenta) or a cold
// one (cyan, blue, violet). Below the horizon it shines through the floor, dimmed by `through`.
// Black where the sky is empty, so it can be screened over another sky.
// @param above float = 0.2 -0.5..1 -- how far the sun's centre stands above the horizon, in picture heights
// @param size float = 0.26 0.02..1 -- the sun's radius, in picture heights
// @param lines int = 8 0..24 -- dark lines across the lower half
// @param glow float = 0.6 0..3 -- the light around the disc
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the lines drift down one place every 8 beats
// @param energy float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the sun brightens and swells a little with it
// @param flash float = 0 0..1 -- a flash, bind it to drop of a chan.beat_grid
// @param warmth float = 0 0..1 -- 0 the warm gradient, 1 the cold one, as the stage's palette
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the sun is there: 1 all of it, 0 nothing (a dark sky)
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (bind it to height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the stage's `end`: the horizon lies there
// @param through float = 1 0..1 -- how much of the sun shines through the floor below the horizon: 1 all of it, 0 none (the sun is cut at the horizon)

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. The horizon is where the floor fades out, `reach` metres ahead, as the
// camera sees it: it moves with the camera's height, pitch, roll and turn, and things in the sky
// keep their size in degrees when the field of view changes. `.z`: one pixel in the same unit.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    // Tangents of the angle from the view's middle, y up.
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan(down)) / unit, 2.0 * t / (res.y * unit));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    // In picture heights of a 60 degree view, x from straight ahead, y up from the horizon.
    let at = sky_at(v.pos.xy);
    let p = at.xy;
    let energy = clamp(params.energy, 0.0, 1.0);
    let flash = clamp(params.flash, 0.0, 1.0);
    let r = max(params.size, 0.001) * (1.0 + 0.03 * energy);
    let q = (p - vec2f(0.0, params.above)) / r;
    let d = length(q);
    // A soft edge one pixel wide.
    let edge = 1.5 * at.z / r;
    var disc = 1.0 - smoothstep(1.0 - edge, 1.0 + edge, d);

    // The lines: in the lower half, each band a little wider than the one above it.
    let below = -q.y;
    if params.lines > 0 && below > 0.0 {
        let n = f32(params.lines);
        let t = below * n + fract(params.beats / 8.0);
        let f = fract(t);
        let cut = mix(0.1, 0.6, clamp(below, 0.0, 1.0));
        let aa = 1.5 * n * at.z / r;
        // Dark where the band begins, lit after `cut`; soft at both ends against aliasing.
        let lit = smoothstep(cut - aa, cut + aa, f) * (1.0 - smoothstep(1.0 - aa, 1.0, f));
        disc *= mix(1.0, lit, smoothstep(0.0, 0.08, below));
    }

    // Top to bottom: warm yellow, orange, magenta; cold cyan, blue, violet.
    let s = clamp(0.5 - 0.5 * q.y, 0.0, 1.0);
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    let colour = mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
    let mid = mix(vec3f(1.0, 0.35, 0.4), vec3f(0.4, 0.35, 1.0), clamp(params.warmth, 0.0, 1.0));

    // The glow falls off outside the disc and is a little stronger on a drop.
    let out = max(d - 1.0, 0.0);
    let halo = (params.glow + 0.8 * flash) * (0.5 * exp(-out * 4.0) + 0.25 * exp(-out * 1.2)) * (1.0 - disc);
    var light = colour * disc * (0.85 + 0.35 * energy + 0.6 * flash) + mid * halo * (0.7 + 0.5 * energy);

    // Below the horizon the floor stands; the sun shines through it by `through`. A thin soft line
    // keeps the cut clean.
    light *= mix(clamp(params.through, 0.0, 1.0), 1.0, smoothstep(-1.5 * at.z, 1.5 * at.z, p.y));
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
