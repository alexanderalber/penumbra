// A city skyline standing on the horizon, over black: two rows of towers, the far one lower and
// dimmer. Their outlines glow in the stage's palette, their windows light up and go out, more
// of them lit the louder the mids. The skyline slides slowly sideways as the world travels, the
// far row at half the pace. The sky is screened, so the towers cannot darken it: their bodies
// carry only a faint glow, and the nebula shows through them a little. Nothing below the
// horizon. Black where the sky is empty, so it can be screened over another sky.
// @param seed int = 1 0..1000 -- picks the towers: another number, another skyline
// @param tall float = 0.12 0.02..0.5 -- the tallest towers' height, in picture heights
// @param slide float = 0.002 -0.05..0.05 -- picture heights the near row slides per beat travelled
// @param windows float = 0.35 0..1 -- share of windows lit in quiet passages
// @param flicker float = 0.7 0..1 -- how much the mids light more windows and how often they change
// @param edge float = 0.8 0..3 -- the outlines' light
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param mid float = 0 0..1 -- the mids (energy_mid of a chan.beat_grid)
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the city is there: 1 all of it, 0 nothing (a dark sky)
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (bind it to height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the stage's `end`: the horizon lies there

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

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

fn hash(a: u32, b: u32, c: u32) -> f32 {
    return f32(pn_hash(vec3u(a, b, c)).x) / 4294967296.0;
}

// Column width of the skyline, in picture heights.
const COLUMN: f32 = 0.045;

struct Tower {
    // Left and right edge, height; 0 height: no tower.
    left: f32,
    right: f32,
    top: f32,
    // A thin mast on top, its height (0: none).
    mast: f32,
    id: u32,
};

fn tower(col: i32, row: u32, tall: f32) -> Tower {
    let id = bitcast<u32>(col);
    let seed = u32(max(params.seed, 0));
    let a = hash(id, seed, row);
    let b = hash(id, seed, row + 10u);
    let c = hash(id, seed, row + 20u);
    var t: Tower;
    let w = mix(0.55, 1.0, b);
    let x0 = f32(col) * COLUMN + (1.0 - w) * COLUMN * c;
    t.left = x0;
    t.right = x0 + w * COLUMN;
    t.top = tall * mix(0.22, 1.0, pow(a, 1.6));
    if a < 0.12 {
        t.top = 0.0;
    }
    t.mast = select(0.0, tall * 0.35 * b, a > 0.86);
    t.id = id * 3u + row;
    return t;
}

// Light of one row at sky point `s` (x already slid); `.w` 1 inside a tower.
fn row_light(s: vec2f, px: f32, row: u32, tall: f32, mid: f32) -> vec4f {
    let col = i32(floor(s.x / COLUMN));
    var light = vec3f(0.0);
    var inside = 0.0;
    for (var k = -1; k <= 1; k++) {
        let t = tower(col + k, row, tall);
        if t.top <= 0.0 {
            continue;
        }
        let dx = max(t.left - s.x, s.x - t.right);
        let dy = s.y - t.top;
        let d = max(dx, dy);
        let body = 1.0 - smoothstep(-px, px, d);
        // The outline: the edge of the body, a line about one and a half pixels wide.
        let line = (1.0 - smoothstep(0.0, 1.5 * px, abs(d))) * step(-px, s.y);
        // The mast: a thin line from the roof up, with a light on top.
        let mx = 0.5 * (t.left + t.right);
        var mast = 0.0;
        if t.mast > 0.0 {
            let up = s.y - t.top;
            mast = (1.0 - smoothstep(0.5 * px, 1.5 * px, abs(s.x - mx))) * step(0.0, up) * step(up, t.mast);
            mast += 1.5 * exp(-length(vec2f(s.x - mx, up - t.mast)) / (2.0 * px)) * (0.5 + 0.5 * sin(params.beats * 3.14159));
        }
        let up = clamp(s.y / tall, 0.0, 1.0);
        light += palette(0.85 - 0.6 * up) * (line * params.edge + mast * 0.7);
        // Windows: a grid on the body, each lit or dark by its own hash; the mids light more,
        // and every few beats some change.
        if body > 0.0 {
            let cell = vec2f(0.0075, 0.009);
            let g = (s - vec2f(t.left, 0.0)) / cell;
            let gi = vec2i(floor(g));
            let f = fract(g);
            let pane = step(0.25, f.x) * step(f.x, 0.75) * step(0.3, f.y) * step(f.y, 0.8);
            let wid = u32(gi.x) * 131u + u32(gi.y) * 7919u;
            let phase = hash(wid, t.id, 3u);
            let epoch = u32(floor(params.beats * mix(0.05, 0.5, clamp(params.flicker, 0.0, 1.0)) + phase * 8.0));
            let share = clamp(params.windows + params.flicker * 0.5 * mid, 0.0, 1.0);
            let lit = step(hash(wid, t.id, epoch), share);
            let margin = step(t.left + 0.004, s.x) * step(s.x, t.right - 0.004) * step(s.y, t.top - 0.006);
            let window_light = mix(vec3f(1.0, 0.78, 0.45), vec3f(0.55, 0.9, 1.0), clamp(params.warmth, 0.0, 1.0));
            light += window_light * pane * lit * margin * body * (0.55 + 0.45 * mid);
            light += palette(0.95) * body * 0.035 * (1.0 - up);
        }
        inside = max(inside, body);
    }
    return vec4f(light, inside);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let s = at.xy;
    let tall = max(params.tall, 0.005);
    let mid = clamp(params.mid, 0.0, 1.0);
    var light = vec3f(0.0);
    if s.y > -at.z && s.y < 2.0 * tall {
        let near = row_light(vec2f(s.x + params.beats * params.slide, s.y), at.z, 0u, tall, mid);
        let far = row_light(vec2f(s.x + params.beats * params.slide * 0.5 + 0.013, s.y), at.z, 1u, tall * 0.62, mid);
        // The near row hides the far one.
        light = near.rgb + far.rgb * 0.45 * (1.0 - near.w);
    }
    light *= smoothstep(-at.z, at.z, s.y);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
