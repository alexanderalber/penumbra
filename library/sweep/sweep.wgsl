// A scan line sweeping across the scene, on everything the scan found (in0: scan_labels'
// `labels`), with a fading trail and the faces it has passed lit faintly in their own tint.
// @param speed float = 0.125 0..1 -- sweeps per second
// @param angle float = 0 -180..180 -- direction of travel, degrees (0 left to right, 90 top down)
// @param line float = 10 1..60 -- width of the bright line, pixels
// @param trail float = 0.25 0..1 -- length of the glow behind it, share of the picture
// @param color color = [0.55, 0.9, 1, 1]
// @param tint float = 0.25 0..1 -- how strongly the passed faces keep a colour of their own
// @param pause float = 0.3 0..2 -- dark time after each sweep, share of one sweep
// @param clock float = -1 -1..600 -- seconds since the effect started (bind a chan.clock); below 0: the show time

// A label of /clean: its red is the face, its green the object, k / 255.
fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

fn hue(h: f32) -> vec3f {
    return clamp(abs(fract(h + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let face = label(in0, p, 0);
    let object = label(in0, p, 1);
    if (face == 0 && object == 0) {
        return vec4f(0.0);
    }
    let size = pn.resolution;
    let a = radians(params.angle);
    let dir = vec2f(cos(a), sin(a));
    // Position along the direction, 0 where the sweep enters the picture, 1 where it leaves.
    let reach = abs(dir.x) * size.x + abs(dir.y) * size.y;
    let s = (dot(v.pos.xy - size * 0.5, dir) + reach * 0.5) / reach;
    let t = select(pn_time.seconds, params.clock, params.clock >= 0.0);
    let cycle = fract(t * params.speed);
    let front = cycle * (1.0 + params.pause + params.trail) - params.trail * 0.5;
    let ahead = (s - front) * reach;
    let core = exp(-ahead * ahead / max(params.line * params.line * 0.25, 0.25));
    let behind = front - s;
    let glow = select(0.0, exp(-behind / max(params.trail, 0.01) * 3.0), behind > 0.0) * 0.35;
    var c = params.color.rgb * (core * 1.5 + glow);
    // Faces passed keep a faint colour of their own, the objects a warm one.
    let seed = select(f32(face) * 0.618, 0.08, object > 0);
    c += hue(fract(seed)) * params.tint * glow * 2.0;
    let alpha = clamp(core + glow, 0.0, 1.0);
    return vec4f(c, alpha);
}
