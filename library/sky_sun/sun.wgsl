// The synthwave sun over black: a big disc standing on the horizon, dark horizontal lines cut
// into its lower half (wider towards the bottom, drifting slowly down), and a glow around it.
// Its colour runs from top to bottom through a warm gradient (yellow, orange, magenta) or a cold
// one (cyan, blue, violet). Nothing below the horizon. Black where the sky is empty, so it can
// be screened over another sky.
// @param horizon float = 0.68 0..1 -- where the horizon lies, 0 the picture's top, 1 its bottom (the stage's floor grid ends at 0.68 with its default fade)
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

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let res = pn.resolution;
    // In picture heights, x from the middle, y up from the horizon.
    let p = vec2f((v.pos.x - 0.5 * res.x) / res.y, params.horizon - v.pos.y / res.y);
    let energy = clamp(params.energy, 0.0, 1.0);
    let flash = clamp(params.flash, 0.0, 1.0);
    let r = max(params.size, 0.001) * (1.0 + 0.03 * energy);
    let q = (p - vec2f(0.0, params.above)) / r;
    let d = length(q);
    // A soft edge one pixel wide.
    let edge = 1.5 / (r * res.y);
    var disc = 1.0 - smoothstep(1.0 - edge, 1.0 + edge, d);

    // The lines: in the lower half, each band a little wider than the one above it.
    let below = -q.y;
    if params.lines > 0 && below > 0.0 {
        let n = f32(params.lines);
        let t = below * n + fract(params.beats / 8.0);
        let f = fract(t);
        let cut = mix(0.1, 0.6, clamp(below, 0.0, 1.0));
        let aa = 1.5 * n / (r * res.y);
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

    // Nothing below the horizon: the floor stands there. A thin soft line keeps the cut clean.
    light *= smoothstep(-1.5 / res.y, 1.5 / res.y, p.y);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
