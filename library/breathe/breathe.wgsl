// Regions (box fronts, tops, walls) each in one colour of a palette, breathing slowly one after
// another from left to right, and now and then a wipe that crosses the picture and lights the
// regions' outlines as it passes.
// in0: the regions, an 8-bit label picture through tex.image (scan.regions' regions_camera.png,
// or a cleaned version of it), 0 off. in1, optional: a 256 x 1 table, per label at x = k red
// the colour slot k / 255 and green the region's phase 0..1 (8-bit through tex.image);
// unconnected, slot k * 3 mod count and the phase from the pixel's place left to right.
// in2, optional: the outlines, white; unconnected, they are found from in0 (`line` wide).
// @param count int = 4 1..4 -- how many of the colours are used
// @param c0 color = [1, 0.5, 0.2, 1]
// @param c1 color = [0.2, 0.6, 0.6, 1]
// @param c2 color = [1, 0.9, 0.6, 1]
// @param c3 color = [0.4, 0.1, 0.1, 1]
// @param edge color = [1, 0.9, 0.7, 1] -- the outlines' colour
// @param gain float = 0.6 0..2 -- the fields' brightness at the top of a breath
// @param base float = 0.35 0..1 -- the fields' brightness between breaths, share of gain
// @param period float = 7 1..30 -- seconds per breath
// @param spread float = 0.6 0..2 -- breaths across the picture, left to right (0: all at once)
// @param fade float = 2 0..10 -- seconds the fields take to come up when the effect starts
// @param first float = 2.5 0..30 -- seconds to the first wipe
// @param every float = 9 2..60 -- seconds from one wipe to the next
// @param wipe float = 1.6 0.2..5 -- seconds a wipe takes across the picture
// @param breadth float = 0.12 0.01..1 -- the wipe's band, share of the picture
// @param angle float = 0 -180..180 -- the first wipe's direction, degrees (0 left to right, 90 top down)
// @param turn float = 180 -180..180 -- added to the direction with every wipe
// @param lines float = 2 0..4 -- the outlines' brightness in the band
// @param lift float = 0.5 0..2 -- how much brighter the fields get in the band
// @param clock float = -1 -1..100000 -- seconds since the effect started (bind a chan.clock); below 0: the show time
// @param line float = 6 1..30 -- the outlines' width when in2 is not connected, pixels

// tex.image decodes its PNGs as sRGB; this undoes that, giving the 8-bit value / 255 back.
fn raw(c: vec4f) -> vec4f {
    let v = max(c.rgb, vec3f(0.0));
    let e = select(1.055 * pow(v, vec3f(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3f(0.0031308));
    return vec4f(e, c.a);
}

fn colour(i: i32) -> vec3f {
    switch i {
        case 0: { return params.c0.rgb; }
        case 1: { return params.c1.rgb; }
        case 2: { return params.c2.rgb; }
        default: { return params.c3.rgb; }
    }
}

// The wipe's band at pixel `pos`, 0..1, 0 between wipes.
fn band(pos: vec2f, t: f32) -> f32 {
    let since = t - params.first;
    if (since < 0.0) {
        return 0.0;
    }
    let k = floor(since / params.every);
    let u = (since - k * params.every) / params.wipe;
    if (u > 1.0) {
        return 0.0;
    }
    let size = pn.resolution;
    let a = radians(params.angle + k * params.turn);
    let dir = vec2f(cos(a), sin(a));
    // Position along the direction, 0 where the band enters the picture, 1 where it leaves.
    let reach = abs(dir.x) * size.x + abs(dir.y) * size.y;
    let s = (dot(pos - size * 0.5, dir) + reach * 0.5) / reach;
    // The band's centre runs from one width before the picture to one width after it, eased.
    let e = u * u * (3.0 - 2.0 * u);
    let centre = mix(-params.breadth, 1.0 + params.breadth, e);
    let d = (s - centre) / max(params.breadth * 0.5, 0.001);
    return exp(-d * d);
}

// The region label at p, 0 outside the picture.
fn region(p: vec2i) -> i32 {
    let size = vec2i(textureDimensions(in0));
    if (any(p < vec2i(0)) || any(p >= size)) {
        return 0;
    }
    return i32(round(raw(textureLoad(in0, p, 0)).r * 255.0));
}

// 1 on a region's outline (another label within `line` pixels), searched only where it shows.
fn outline(p: vec2i, k: i32) -> f32 {
    let r = i32(ceil(params.line));
    let step = max(r / 3, 1);
    for (var dy = -r; dy <= r; dy += step) {
        for (var dx = -r; dx <= r; dx += step) {
            if (dx * dx + dy * dy <= r * r && region(p + vec2i(dx, dy)) != k) {
                return 1.0;
            }
        }
    }
    return 0.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let k = i32(round(raw(textureLoad(in0, p, 0)).r * 255.0));
    if (k <= 0) {
        return vec4f(0.0);
    }
    // Unconnected inputs are a single transparent pixel.
    let table = textureDimensions(in1).x >= 256u;
    var entry = raw(textureLoad(in1, vec2i(k, 0), 0));
    if (!table) {
        entry = vec4f(f32((k * 3) % 4) / 255.0, v.pos.x / pn.resolution.x, 0.0, 1.0);
    }
    let slot = i32(round(entry.r * 255.0)) % max(params.count, 1);
    let t = select(pn_time.seconds, params.clock, params.clock >= 0.0);
    let up = smoothstep(0.0, max(params.fade, 0.001), t);
    // A breath: base up to full and back, each region a little later than the one to its left.
    let breath = 0.5 - 0.5 * cos(6.2831853 * (t / params.period - entry.g * params.spread));
    let level = params.gain * mix(params.base, 1.0, breath) * up;
    let b = band(v.pos.xy, t);
    var c = colour(slot) * level * (1.0 + params.lift * b);
    var edge = raw(textureLoad(in2, p, 0)).r;
    if (textureDimensions(in2).x <= 1u) {
        edge = select(0.0, outline(p, k), b > 0.001);
    }
    let line = edge * b * params.lines;
    c = mix(c, params.edge.rgb, clamp(line, 0.0, 1.0));
    return vec4f(c, 1.0);
}
