// A head-up display drawn over the finished picture of a stage, in thin lines of the palette's
// colour: a crosshair in the middle, the horizon with a pitch ladder, an altimeter tape with the
// camera's height, a bar counter with a tick per beat, and a radar. The radar's sweep turns once
// per bar; its blips are the sections of the track, the one playing lit, the ones played dimmer,
// and its rim fills as the track goes on. The lines flicker on every onset and flash on a drop.
// They are added to the picture's light, so nothing is covered.
// in0: the picture. in1: the height as text, in2 the bar count, in3 the track's number (tex.text
// pictures of any size, laid over the whole picture).
// The horizon and the ladder follow the camera when eye, pitch, roll, fov and side are bound to
// a stage's `pose`; unbound they stand where the stage's first camera puts them.
// @param presence float = 1 0..1 -- how bright the display is: 1 full, 0 the picture unchanged
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..4 -- how bright the lines are
// @param thickness float = 2 0.5..6 -- line width in pixels of a 1080 line picture
// @param flicker float = 0.5 0..1 -- how much the lines dim and jitter on an onset
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bar float = 0 0..1 -- how far through the bar (bar of a chan.beat_grid)
// @param beats_per_bar float = 4 2..8 -- beats in a bar, for the ticks
// @param phase float = 0 0..1 -- how far through the track (track_phase of a chan.beat_grid)
// @param sections float = 1 0..64 -- sections in the track (track_sections of a chan.beat_grid)
// @param section float = 0 0..64 -- the section playing (track_section of a chan.beat_grid)
// @param flash float = 0 0..1 -- a flicker, bind it to a decaying onset of a chan.beat_grid
// @param drop float = 0 0..1 -- a flash of the whole display (drop of a chan.beat_grid)
// @param eye float = 1.5 0..10 -- the camera's height in metres (height of a stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt in degrees, negative down (pitch of a stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view in degrees (fov of a stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll in degrees (roll of a stage's pose)
// @param side float = 0 -45..45 -- the camera's look to the side in degrees (side of a stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the stage's floor ends, as its fade's end
// @param scale float = 0.1 0.02..0.5 -- picture heights of the altimeter tape per metre

const TAU: f32 = 6.2831853;

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

// Where a pixel looks, in tangents of a 60 degree view from the horizon straight ahead: x
// right, y up, z the size of a pixel in the same unit. As the sky elements place the horizon.
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

// Distance from p to the segment a..b.
fn segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - a - ab * t);
}

// A line of half width w (pixels) at distance d, with a soft pixel edge.
fn ink(d: f32, w: f32) -> f32 {
    return 1.0 - smoothstep(w - 0.5, w + 0.5, d);
}

fn text(t: u32, uv: vec2f) -> f32 {
    if t == 1u {
        return textureSampleLevel(in1, pn_clamp, uv, 0.0).a;
    }
    if t == 2u {
        return textureSampleLevel(in2, pn_clamp, uv, 0.0).a;
    }
    return textureSampleLevel(in3, pn_clamp, uv, 0.0).a;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let c = textureLoad(in0, vec2i(v.pos.xy), 0);
    let presence = clamp(params.presence, 0.0, 1.0);
    if presence <= 0.0 {
        return c;
    }
    let res = pn.resolution;
    let h = res.y;
    let k = h / 1080.0;
    let w = 0.5 * max(params.thickness, 0.5) * k;
    let p = v.pos.xy;
    let mid = 0.5 * res;
    var light = 0.0;

    // Crosshair: a broken ring, four ticks and a dot.
    let r0 = 0.028 * h;
    let d = p - mid;
    let ang = atan2(d.y, d.x);
    let broken = step(0.18, abs(fract(ang / TAU * 4.0 + 0.125) - 0.5) * 2.0);
    light += ink(abs(length(d) - r0), w) * broken;
    let t0 = 0.042 * h;
    let t1 = 0.07 * h;
    light += ink(segment(p, mid + vec2f(t0, 0.0), mid + vec2f(t1, 0.0)), w);
    light += ink(segment(p, mid - vec2f(t0, 0.0), mid - vec2f(t1, 0.0)), w);
    light += ink(segment(p, mid + vec2f(0.0, t0), mid + vec2f(0.0, t1)), w);
    light += ink(segment(p, mid - vec2f(0.0, t0), mid - vec2f(0.0, t1)), w);
    light += ink(length(d), 1.6 * w);

    // Horizon and pitch ladder: lines at fixed angles above and below the horizon, broken in
    // the middle so they leave the crosshair free.
    let s = sky_at(p);
    let px = max(s.z, 1e-6);
    let ax = abs(s.x);
    light += ink(abs(s.y) / px, w) * step(0.16, ax) * step(ax, 0.62);
    for (var i = 1; i <= 2; i++) {
        let lvl = f32(i) * 0.12;
        let len = 0.11 - 0.025 * f32(i);
        let band = step(0.07, ax) * step(ax, 0.07 + len);
        // Dashed below the horizon, solid above, as in a flight display.
        let dash = step(0.5, fract(ax / 0.02));
        light += 0.6 * ink(abs(s.y - lvl) / px, w) * band;
        light += 0.6 * ink(abs(s.y + lvl) / px, w) * band * dash;
    }

    // Altimeter: a tape on the right, a tick every quarter metre, a long one per metre, the
    // reading beside a caret in the middle.
    let x0 = res.x - 0.09 * h;
    let top = 0.3 * h;
    let bottom = 0.7 * h;
    let inside = step(top, p.y) * step(p.y, bottom);
    light += 0.7 * ink(abs(p.x - x0), w) * inside;
    let per = max(params.scale, 0.01) * h;
    let metres = params.eye + (0.5 * h - p.y) / per;
    let q = metres / 0.25;
    let near = abs(q - round(q)) * 0.25 * per;
    let major = (i32(round(q)) % 4) == 0;
    let reach = select(0.012, 0.026, major) * h;
    let fade = 1.0 - smoothstep(0.12 * h, 0.2 * h, abs(p.y - 0.5 * h));
    light += ink(near, w) * step(x0, p.x) * step(p.x, x0 + reach) * inside * fade;
    let cy = 0.5 * h;
    let tip = vec2f(x0 - 0.006 * h, cy);
    light += ink(segment(p, tip, tip + vec2f(-0.02 * h, -0.012 * h)), w);
    light += ink(segment(p, tip, tip + vec2f(-0.02 * h, 0.012 * h)), w);
    light += text(1u, p / res);

    // Bar counter: the number top left, a box per beat below it, filled up to the beat playing.
    light += text(2u, p / res);
    let bpb = clamp(round(params.beats_per_bar), 1.0, 8.0);
    let now = floor(clamp(params.bar, 0.0, 0.9999) * bpb);
    let box = 0.016 * h;
    for (var b = 0.0; b < bpb; b += 1.0) {
        let centre = vec2f(0.05 * h + b * 2.2 * box + box, 0.13 * h);
        let e = abs(p - centre) - vec2f(box);
        let outline = ink(abs(max(e.x, e.y)), w);
        let filled = select(0.0, select(0.35, 0.9, b == now), b <= now) * step(max(e.x, e.y), 0.0);
        light += outline + filled;
    }

    // Radar, bottom left: rings, a sweep turning once per bar with a fading trail, a blip per
    // section of the track, and the rim lit as far as the track has come.
    let rc = vec2f(0.17 * h, h - 0.17 * h);
    let rr = 0.12 * h;
    let rd = p - rc;
    let rl = length(rd);
    // Angle clockwise from straight up, 0 to 1.
    let ra = fract(atan2(rd.x, -rd.y) / TAU + 1.0);
    let disc = step(rl, rr);
    light += 0.8 * ink(abs(rl - rr), w);
    light += 0.35 * ink(abs(rl - 0.5 * rr), w);
    light += 0.25 * (ink(abs(rd.x), w) + ink(abs(rd.y), w)) * disc;
    let sweep = fract(params.bar);
    let behind = fract(sweep - ra);
    light += 0.9 * ink(segment(p, rc, rc + rr * vec2f(sin(TAU * sweep), -cos(TAU * sweep))), w);
    light += 0.22 * exp(-behind * 9.0) * disc;
    let done = step(ra, clamp(params.phase, 0.0, 1.0));
    light += 1.2 * done * ink(abs(rl - rr - 3.0 * w), 1.2 * w);
    let n = clamp(round(params.sections), 1.0, 64.0);
    let playing = clamp(round(params.section), 0.0, n - 1.0);
    let slot = min(floor(ra * n), n - 1.0);
    let ba = TAU * (slot + 0.5) / n;
    let bc = rc + 0.68 * rr * vec2f(sin(ba), -cos(ba));
    let bd = length(p - bc);
    let pulse = 0.5 + 0.5 * cos(TAU * fract(params.beats));
    let lit = exp(-fract(sweep - (slot + 0.5) / n) * 5.0);
    let br = 0.085 * rr;
    if slot == playing {
        light += 1.4 * ink(bd, br * (1.0 + 0.25 * pulse)) + 0.8 * pulse * ink(abs(bd - 2.0 * br), w);
    } else if slot < playing {
        light += (0.45 + 0.5 * lit) * ink(bd, br);
    } else {
        light += (0.3 + 0.4 * lit) * ink(abs(bd - br), w);
    }
    light += text(3u, p / res);

    // Flicker on an onset: dimmer, and a few frames with lines missing; a flash on a drop.
    let flash = clamp(params.flash, 0.0, 1.0) * clamp(params.flicker, 0.0, 1.0);
    let hiccup = f32(pn_hash(vec3u(u32(fract(params.beats / 64.0) * 4096.0), 3u, 1u)).x) / 4294967296.0;
    let dim = (1.0 - 0.45 * flash) * select(1.0, 0.35, flash > 0.4 && hiccup < 0.4);
    let boost = 1.0 + 1.5 * clamp(params.drop, 0.0, 1.0);
    let colour = palette(0.12);
    let add = colour * clamp(light, 0.0, 1.6) * params.brightness * dim * boost * presence;
    let a = max(c.a, clamp(max(add.r, max(add.g, add.b)), 0.0, 1.0));
    return vec4f(c.rgb + add, a);
}
