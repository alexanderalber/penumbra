// A night sky of stars over black: three layers of stars by `seed` (many faint ones, fewer
// medium ones, a few bright ones), in patches of denser and sparser sky, and Venus as the
// brightest point, low over the horizon. The treble makes the stars twinkle. Towards `dawn` 1 the
// stars thin out near the horizon; Venus stays. Nothing below the horizon. The stars keep their
// place in the sky when the camera moves.
// @param presence float = 1 0..1 -- how much of the sky is there: 1 all of it, 0 nothing
// @param brightness float = 1 0..3 -- the light of all stars
// @param seed int = 0 0..1000 -- picks another star field
// @param density float = 1 0..3 -- how many stars: 1 the usual sky, 0 only the brightest
// @param size float = 0.8 0.3..3 -- a star's size, in pixels
// @param twinkle float = 1 0..2 -- how much the treble makes the stars twinkle; 0 they stand still
// @param venus float = 1 0..3 -- Venus's light; 0 no Venus
// @param venus_x float = -0.32 -1.5..1.5 -- where Venus stands from straight ahead, in picture heights (left negative)
// @param venus_y float = 0.1 0..1 -- how high Venus stands above the horizon, in picture heights
// @param dawn float = 0 0..1 -- the stage's dawn (bind it to dawn of the stage's pose): the stars near the horizon fade towards 1
// @param thin float = 0.35 0.01..2 -- how high above the horizon the dawn thins the stars, in picture heights
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the twinkle's clock
// @param sparkle float = 0 0..1 -- the treble (energy_high of a chan.beat_grid): how much twinkle there is now
// @param eye float = 1.2 0..10 -- the camera's height over the floor, metres (height of the stage's pose)
// @param pitch float = -1 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 58 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 40 1..200 -- metres ahead where the floor fades out, the stage's `end` (far of the pose): the horizon lies there

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. `.z`: one pixel in the same unit.
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

fn rand(c: vec2i, layer: u32, k: u32) -> vec3f {
    let h = pn_hash(vec3u(bitcast<u32>(c.x), bitcast<u32>(c.y), layer * 131u + k + u32(max(params.seed, 0)) * 977u));
    return vec3f(h) / 4294967296.0;
}

// The light of one layer of stars at sky place p: cells of `cell` picture heights, each holding
// a star with chance `chance`, its brightness drawn to the power `power`.
fn star_layer(p: vec2f, px: f32, cell: f32, chance: f32, power: f32, gain: f32, id: u32) -> vec3f {
    let g = p / cell;
    let c = vec2i(floor(g));
    let a = rand(c, id, 0u);
    if a.x >= chance {
        return vec3f(0.0);
    }
    let b = rand(c, id, 1u);
    // The star's place in its cell, away from the edges so its glow stays inside.
    let at = (vec2f(c) + 0.2 + 0.6 * b.xy) * cell;
    let sigma = max(params.size, 0.3) * px;
    let d = length(p - at);
    let spot = exp(-0.5 * d * d / (sigma * sigma));
    if spot < 0.002 {
        return vec3f(0.0);
    }
    var light = pow(a.y, power) * gain;
    // Twinkle: each star at its own rate, a few times a second at dance tempo.
    let tw = clamp(params.twinkle, 0.0, 2.0) * clamp(params.sparkle, 0.0, 1.0);
    let phase = sin(6.2831853 * (params.beats * mix(0.6, 2.2, b.z) + a.z));
    light *= max(1.0 + 0.6 * tw * phase, 0.0);
    // Colour: a little blue, white or a little warm.
    let colour = mix(mix(vec3f(0.70, 0.82, 1.0), vec3f(1.0, 1.0, 1.0), smoothstep(0.0, 0.5, a.z)),
                     vec3f(1.0, 0.82, 0.62), smoothstep(0.6, 1.0, a.z));
    return colour * light * spot;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let p = at.xy;
    let px = max(at.z, 1e-5);
    if p.y < -2.0 * px {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let s = f32(max(params.seed, 0));
    // Patches of denser sky.
    let clump = 0.5 + 0.5 * pn_noise(vec3f(p * 2.2, s * 0.37), 13u);
    let dense = max(params.density, 0.0) * mix(0.35, 1.6, clump);
    var light = star_layer(p, px, 0.0065, 0.22 * dense, 3.0, 0.35, 1u);
    light += star_layer(p, px, 0.021, 0.40 * min(dense, 2.5), 2.5, 0.8, 2u);
    light += star_layer(p, px, 0.065, 0.55 * min(params.density, 1.0), 1.6, 1.6, 3u);

    // The dawn thins them near the horizon; all stars dim a little low down, as through more air.
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let up = max(p.y, 0.0);
    let thinned = 1.0 - dawn * exp(-up / max(params.thin, 0.01));
    let air = smoothstep(0.0, 0.06, up);
    light *= thinned * mix(0.4, 1.0, air) * (1.0 - 0.25 * dawn);

    // Venus: steady and bright, with a small halo.
    if params.venus > 0.0 {
        let d = length(p - vec2f(params.venus_x, params.venus_y));
        let sigma = 1.3 * max(params.size, 0.3) * px;
        let disc = exp(-0.5 * d * d / (sigma * sigma));
        let halo = 0.05 * exp(-d / (10.0 * px));
        light += vec3f(1.0, 0.94, 0.84) * params.venus * (1.6 * disc + halo);
    }

    let edge = smoothstep(-px, px, p.y);
    let out = light * max(params.brightness, 0.0) * clamp(params.presence, 0.0, 1.0) * edge;
    return vec4f(out, 1.0);
}
