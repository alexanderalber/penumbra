// The music's levels as a row of glowing bars standing on the horizon, over black: the bass in
// the middle, the mids beside it, the treble at both ends. Each bar is a stack of cells and
// moves a little by itself, so the row reads as an equaliser. The bus carries levels, not a
// spectrum: the bars between the bands blend them. An onset lifts every bar for a moment.
// Nothing below the horizon. Black where the sky is empty, so it can be screened over another
// sky.
// @param count int = 24 4..64 -- bars in the row
// @param span float = 0.62 0.05..2 -- half the row's width, in picture heights
// @param tall float = 0.12 0.01..0.6 -- a bar's height at full level, in picture heights
// @param gap float = 0.3 0..0.9 -- share of a bar's place left dark between two bars
// @param cells int = 9 0..40 -- cells a full bar is stacked of; 0 draws solid bars
// @param jitter float = 0.5 0..1 -- how much each bar moves by itself
// @param kick float = 0.25 0..1 -- how much an onset lifts the bars
// @param glow float = 0.5 0..3 -- the light around the bars
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param low float = 0 0..1 -- the bass (energy_low of a chan.beat_grid)
// @param mid float = 0 0..1 -- the mids (energy_mid)
// @param high float = 0 0..1 -- the treble (energy_high)
// @param onset float = 0 0..1 -- 1 in the frame of an onset (onset of a chan.beat_grid)
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the row is there: 1 all of it, 0 nothing (a dark sky)
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (bind it to height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the stage's `end`: the horizon lies there
// @param bend float = 0 0..1 -- how far the horizon bends into a ring, for a tube: 0 straight, 1 the ring of the tube's far end, so the row stands on its wall (curve of the stage's pose)
// @param axis float = 4 0..20 -- the tube's axis, metres over the floor (axis of the stage's pose)
// @param radius float = 4 0..20 -- the tube's radius, metres (radius of the stage's pose)
// @param far float = 20 1..100 -- metres ahead where the tube fades out: its far end lies there (far of the stage's pose)

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

// The tube's far end as the camera sees it, in sky_at's units: `.xy` its middle, `.z` its radius.
// The end is the ring of `radius` round the tube's axis, `axis` metres up and `far` metres ahead.
// The camera sits below the axis, so the middle lies above the vanishing point.
fn tube_end() -> vec3f {
    let unit = 2.0 * tan(radians(30.0));
    let pitch = radians(params.pitch);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + pitch;
    let far = max(params.far, 0.5);
    let r = max(params.radius, 0.0);
    let top = tan(atan((params.axis + r - params.eye) / far) - pitch);
    let bottom = tan(atan((params.axis - r - params.eye) / far) - pitch);
    return vec3f(0.0, (0.5 * (top + bottom) + tan(down)) / unit, max(0.5 * (top - bottom) / unit, 0.02));
}

// The sky point `at` (from sky_at) with the horizon bent into a ring, for a stage that is a
// tube: at `bend` 1 the horizon is the ring of the tube's far end (tube_end), and up is towards
// its middle. Between, the horizon is an arc of a larger ring through the far end's bottom, so
// the row wraps round bit by bit.
fn bent(at: vec3f) -> vec3f {
    let b = clamp(params.bend, 0.0, 1.0);
    if b < 0.001 {
        return at;
    }
    let end = tube_end();
    let radius = end.z / b;
    let centre = vec2f(0.0, b * (end.y - end.z) + radius);
    let d = at.xy - centre;
    let angle = atan2(d.x, -d.y);
    return vec3f(angle * radius, radius - length(d), at.z);
}

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

fn hash(a: u32, b: u32, c: u32) -> f32 {
    return f32(pn_hash(vec3u(a, b, c)).x) / 4294967296.0;
}

// Bar `i`'s level, 0 to 1.
fn level(i: i32, n: i32) -> f32 {
    let b = abs((f32(i) + 0.5) / f32(n) * 2.0 - 1.0);
    let band = mix(mix(params.low, params.mid, smoothstep(0.1, 0.55, b)), params.high, smoothstep(0.55, 0.95, b));
    let wobble = pn_noise(vec3f(f32(i) * 1.37, params.beats * 1.3, 0.5), 21u);
    let own = clamp(1.0 + params.jitter * wobble, 0.0, 1.6);
    return clamp(clamp(band, 0.0, 1.0) * own + params.kick * clamp(params.onset, 0.0, 1.0) * (0.6 + 0.4 * hash(u32(i), 7u, 1u)), 0.0, 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = bent(sky_at(v.pos.xy));
    let s = at.xy;
    let n = max(params.count, 1);
    let span = max(params.span, 0.01);
    let pitch = 2.0 * span / f32(n);
    let f = (s.x + span) / pitch;
    let i = i32(floor(f));
    var light = vec3f(0.0);
    if s.y > -at.z {
        // The bar under the pixel and its two neighbours, for the glow.
        for (var k = -1; k <= 1; k++) {
            let j = i + k;
            if j < 0 || j >= n {
                continue;
            }
            let h = max(params.tall, 0.001) * level(j, n);
            let half = 0.5 * pitch * (1.0 - clamp(params.gap, 0.0, 0.9));
            let cx = -span + (f32(j) + 0.5) * pitch;
            let dx = abs(s.x - cx) - half;
            let dy = max(s.y - h, -s.y);
            let d = length(max(vec2f(dx, dy), vec2f(0.0)));
            let inside = 1.0 - smoothstep(-at.z, at.z, max(dx, dy));
            var lit = inside;
            if params.cells > 0 {
                let cell = max(params.tall, 0.001) / f32(params.cells);
                let c = fract(s.y / cell);
                let aa = at.z / cell;
                lit *= smoothstep(0.22 - aa, 0.22 + aa, c);
            }
            let up = clamp(s.y / max(params.tall, 0.001), 0.0, 1.0);
            let colour = palette(1.0 - up);
            light += colour * lit * 1.1;
            light += colour * params.glow * 0.22 * exp(-max(d, 0.0) / (0.12 * pitch)) * (1.0 - inside) * smoothstep(0.0, 0.02, h);
        }
    }
    light *= smoothstep(-at.z, at.z, s.y);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
