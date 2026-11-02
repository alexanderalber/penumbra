// Curtains of light over the horizon, over black: `bands` curtains hang one above the other,
// each a bright lower hem that folds and sways, with rays of light rising from it and fading
// upwards. They drift sideways with the distance travelled and brighten with the treble. The
// colour runs from the hem upwards through a warm gradient (orange, pink, magenta) or a cold
// one (green, cyan, violet). Nothing below the horizon. Black where the sky is empty, so it can
// be screened over another sky.
// @param horizon float = 0.68 0..1 -- where the horizon lies, 0 the picture's top, 1 its bottom (the stage's floor grid ends at 0.68 with its default fade)
// @param bands int = 3 1..5 -- curtains, one above the other
// @param hang float = 0.22 0..1 -- how high the lowest curtain hangs above the horizon, in picture heights; the others hang above it
// @param speed float = 0.02 0..0.5 -- how fast the curtains drift and fold, per beat travelled
// @param glow float = 1 0..3 -- the curtains' light
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param energy float = 0 0..1 -- the treble (energy_high of a chan.beat_grid): the curtains brighten with it
// @param warmth float = 0 0..1 -- 0 the warm gradient, 1 the cold one, as the stage's palette
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the aurora is there: 1 all of it, 0 nothing (a dark sky)

fn hash1(i: f32, seed: u32) -> f32 {
    return f32(pn_hash(vec3u(bitcast<u32>(i32(i)), seed, 3u)).x) / 4294967296.0;
}

// Smooth noise along one axis.
fn noise1(x: f32, seed: u32) -> f32 {
    let i = floor(x);
    let f = x - i;
    let u = f * f * (3.0 - 2.0 * f);
    return mix(hash1(i, seed), hash1(i + 1.0, seed), u);
}

fn fbm1(x: f32, seed: u32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = x;
    for (var k = 0u; k < 4u; k++) {
        sum += amp * noise1(q, seed + k * 13u);
        q = q * 2.07 + 3.1;
        amp *= 0.5;
    }
    return sum;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let res = pn.resolution;
    // In picture heights, x from the middle, y up from the horizon.
    let p = vec2f((v.pos.x - 0.5 * res.x) / res.y, params.horizon - v.pos.y / res.y);
    if p.y < -0.01 {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let t = params.beats * params.speed;
    let warmth = clamp(params.warmth, 0.0, 1.0);
    let n = clamp(params.bands, 1, 5);
    var light = vec3f(0.0);
    for (var b = 0; b < n; b++) {
        let fb = f32(b);
        let seed = u32(b) * 101u + 7u;
        // The curtain folds sideways: x is pushed by a slow wave before the hem and the rays read it.
        let fold = 0.12 * (fbm1(p.x * 1.3 + t * 0.6 + fb * 5.0, seed) - 0.5);
        let x = p.x + fold + t * (0.25 + 0.1 * fb);
        // The hem: higher curtains hang further up, each swaying in its own way.
        let hem = params.hang * (1.0 + 0.7 * fb) + 0.16 * (fbm1(x * 1.1 + fb * 9.0, seed + 1u) - 0.5) + 0.03 * sin(x * 4.0 + fb * 2.0);
        let above = p.y - hem;
        // Bright just above the hem, fading over a tall curtain; a sharp edge below it.
        let reach = 0.09 + 0.04 * fb;
        let curtain = select(exp(above * 60.0), exp(-above / reach), above >= 0.0);
        // Rays: fine streaks along the curtain, flickering slowly.
        let rays = 0.15 + 0.85 * pow(noise1(x * 38.0 + t * 2.0, seed + 2u), 2.0);
        // Gaps along the curtain, so it does not span the whole sky.
        let spread = smoothstep(0.35, 0.65, fbm1(x * 0.8 - fb * 3.0, seed + 3u));
        let s = clamp(above / (2.5 * reach), 0.0, 1.0);
        let warm = mix(vec3f(1.0, 0.5, 0.25), vec3f(0.85, 0.15, 0.7), s);
        let cold = mix(vec3f(0.2, 1.0, 0.55), vec3f(0.5, 0.25, 1.0), s);
        let fade = 1.0 / (1.0 + 0.45 * fb);
        light += mix(warm, cold, warmth) * curtain * rays * spread * fade;
    }
    let lift = 0.7 + 1.0 * clamp(params.energy, 0.0, 1.0);
    light *= params.glow * lift;
    light *= smoothstep(-0.01, 0.01, p.y);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
