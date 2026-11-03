// Pairs of eyes in the dark sky, over black. Each eye is a glowing almond with an iris in the
// stage's palette and a dark pupil. The pupils follow the lane: they look the way it turns. Now
// and then a pair blinks, shut for about four frames, on a bar its own number picks. Meant for
// breaks and intros. Nothing below the horizon. Black where the sky is empty, so it can be
// screened over another sky.
// in0: the stage's lane (its `lane` output), optional: without it the eyes follow `drift`.
// @param pairs int = 3 1..6 -- pairs of eyes
// @param seed int = 1 0..1000 -- picks where the pairs sit and when they blink
// @param size float = 0.045 0.005..0.3 -- an eye's half width, in picture heights
// @param look float = 0.3 0..0.6 -- how far the pupils move, in eye widths
// @param blinks float = 0.35 0..1 -- share of bars on which a pair blinks
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bpm float = 120 20..300 -- beats per minute travelled (travel_bpm): a blink lasts the same time at every tempo
// @param drift float = 0 -1000..1000 -- drift of a chan.beat_grid: the pupils swing with it when no lane is wired
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the eyes is there: 1 all of it, 0 nothing (a dark sky)
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

// Where the lane is looked at: 6 m ahead, texel (6 + 8) / 0.25 of the lane picture.
const LANE_U: f32 = 56.5 / 256.0;
// Seconds a blink takes to close, stays shut (about four frames at 60 fps), and opens.
const CLOSING: f32 = 0.05;
const SHUT: f32 = 0.067;
const OPENING: f32 = 0.06;

// How far a pair's lids are open, 0 to 1, by the blink of the current bar.
fn open_of(pair: u32, seed: u32) -> f32 {
    let bar = floor(params.beats / 4.0);
    let b = u32(max(bar, 0.0));
    if hash(pair, b, seed + 40u) >= params.blinks {
        return 1.0;
    }
    let start = bar * 4.0 + 3.0 * hash(pair, b, seed + 41u);
    let t = (params.beats - start) * 60.0 / max(params.bpm, 20.0);
    if t < 0.0 {
        return 1.0;
    }
    let closing = smoothstep(0.0, CLOSING, t);
    let opening = smoothstep(CLOSING + SHUT, CLOSING + SHUT + OPENING, t);
    return 1.0 - closing + opening;
}

// One eye at `p` (eye half widths, from its middle), lids `open`, pupils moved by `gaze`.
fn eye(p: vec2f, px: f32, open: f32, gaze: vec2f) -> vec3f {
    let x = p.x;
    // The almond: half its height at x, closing towards the corners.
    let lid = 0.48 * pow(max(1.0 - x * x, 0.0), 0.75) * max(open, 0.0);
    let edge = abs(p.y) - lid;
    let inside = (1.0 - smoothstep(-px, px, edge)) * step(abs(x), 1.0);
    // The outline glows; when shut it is one line. Beyond the corners the distance is to the
    // corner itself.
    let line = select(length(vec2f(abs(x) - 1.0, p.y)), abs(edge), abs(x) <= 1.0);
    let out = select(length(vec2f(abs(x) - 1.0, p.y)), max(edge, 0.0), abs(x) <= 1.0);
    let ring = exp(-line / (1.8 * px)) + 0.12 * exp(-out / (12.0 * px));
    let iris_at = gaze;
    let r = length(p - iris_at);
    let iris = 1.0 - smoothstep(0.36 - px, 0.36 + px, r);
    let pupil = 1.0 - smoothstep(0.15 - px, 0.15 + px, r);
    let shine = exp(-length(p - iris_at - vec2f(-0.12, 0.12)) / (1.5 * px)) * 0.8;
    let iris_light = palette(0.55 + 0.4 * r / 0.36) * (1.1 - 0.6 * r / 0.36);
    let white = palette(0.05) * 0.07;
    let fill = mix(white, iris_light, iris) * (1.0 - pupil) + vec3f(shine);
    return fill * inside + palette(0.3) * ring * 0.9;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let s = at.xy;
    let seed = u32(max(params.seed, 0));
    let n = clamp(params.pairs, 1, 6);
    let size = max(params.size, 0.005);
    // The lane's x 6 m ahead (metres; blue is the step, 0 when nothing is wired).
    let lane = textureSampleLevel(in0, pn_clamp, vec2f(LANE_U, 0.5), 0.0);
    var side = clamp(lane.r / 2.5, -1.0, 1.0);
    if lane.b <= 0.0 {
        side = sin(3.14159265 * params.drift);
    }
    let gaze = vec2f(side * params.look, -0.12);
    var light = vec3f(0.0);
    for (var k = 0; k < n; k++) {
        let pair = u32(k);
        let jx = hash(pair, seed, 1u) - 0.5;
        let jy = hash(pair, seed, 2u);
        let scale = mix(0.7, 1.3, hash(pair, seed, 3u));
        let centre = vec2f(mix(-0.7, 0.7, (f32(k) + 0.5) / f32(n)) + 0.35 * jx / f32(n), 0.14 + 0.2 * jy);
        let w = size * scale;
        let open = open_of(pair, seed);
        let q = (s - centre) / w;
        if abs(q.y) > 6.0 || abs(q.x) > 8.0 {
            continue;
        }
        let px = at.z / w;
        light += eye(q - vec2f(-1.55, 0.0), px, open, gaze) + eye(q - vec2f(1.55, 0.0), px, open, gaze);
    }
    light *= smoothstep(-at.z, at.z, s.y);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
