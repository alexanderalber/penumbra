// An endless corridor: rings, rails and rotating gates repeat along a winding path, and the
// camera flies through them. It animates with nothing wired in.
//
// `energy`, `flash` and `hue` can follow the music directly. For flight that follows the music,
// bind `travel` to a channel that only grows (a chan.integrator of the level).
// @param energy float = 0.4 0..1 -- brightness of the lights and of the glow around them. Good for the music's level.
// @param flash float = 0 0..1 -- extra light on every ring at once. Good for a beat.
// @param hue float = 0 0..1 -- moves the lights along the palette.
// @param colours palette = neon -- colours of the lights. With no palette, a built-in rainbow.
// @param speed float = 1.2 0..8 -- rings passed per second. Changing it makes the camera jump, so set it once and bind `travel` instead.
// @param travel float = 0 0..1000 -- rings added to the flight. Bind a channel that only grows.
// @group shape
// @param sides int = 8 3..16 -- walls around the corridor. 4 is a square shaft.
// @param bend float = 1 0..3 -- how far the path winds. 0 is straight.
// @param twist float = 0.6 0..3 -- how far the walls turn to and fro along the path, in radians.
// @param spin float = 0 -3.2..3.2 -- turns the whole corridor around the camera, in radians.
// @param gate float = 1.45 1.1..1.8 -- half the opening of the gates. Above 1 the camera fits through.
// @group camera
// @param camera vec3 = [0, 0, 0] -- camera position from the centre of the path: sideways, up, forward. Stay below 1 sideways and up to clear the gates.
// @param aim vec3 = [0, 0, 8] -- the point the camera looks at, from the centre of the path: sideways, up, distance ahead.
// @param roll float = 0 -180..180 -- turns the picture, in degrees.
// @param fov float = 85 30..130 -- vertical field of view in degrees.
// @group look
// @param steps int = 96 16..200 -- most march steps per pixel. More reaches further at higher cost.
// @param gloss float = 0.5 0..1 -- how much the walls mirror the lights. 0 turns the reflection off and costs less.
// @param haze float = 0.5 0..1 -- how quickly the corridor fades into the distance.

const TAU: f32 = 6.2831853;
const CELL: f32 = 4.0;     // length of one ring's section
const CELLS: f32 = 120.0;  // rings in one lap of the path
const LOOP: f32 = 480.0;   // CELL * CELLS: the path repeats exactly after this length
const RADIUS: f32 = 2.4;
const REACH: f32 = 36.0;   // the fog has closed this far out
const MAX_STEPS: i32 = 240;      // upper limit whatever `steps` says, for a bounded cost
const MIRROR_STEPS: i32 = 16;

// Values that hold for the whole pixel, worked out once in `main`.
var<private> anim: f32;
var<private> gain: f32;
var<private> sides: f32;
var<private> gate_open: f32;
var<private> step_scale: f32;

fn wrap(x: f32, y: f32) -> f32 {
    return x - y * floor(x / y);
}

fn rot(p: vec2f, a: f32) -> vec2f {
    let c = cos(a);
    let s = sin(a);
    return vec2f(c * p.x - s * p.y, s * p.x + c * p.y);
}

fn hash11(x: f32) -> f32 {
    return fract(sin(x * 91.3458) * 47453.5453);
}

// Centre of the corridor at depth z. Whole numbers of waves fit into LOOP, so the path closes.
fn path(z: f32) -> vec2f {
    let w = TAU / LOOP;
    return params.bend * vec2f(
        1.6 * sin(z * w * 5.0) + 0.7 * sin(z * w * 13.0),
        1.1 * cos(z * w * 7.0) + 0.5 * sin(z * w * 17.0),
    );
}

struct Parts {
    wall: f32,
    rib: f32,
    rail: f32,
    gate: f32,
    ring_light: f32,
    gate_light: f32,
    ring_id: f32,
    gate_id: f32,
    along: f32,  // position along the wall, across the corridor
    z_cell: f32,
    sector: f32,
}

fn parts(p: vec3f) -> Parts {
    var s: Parts;
    var q = p.xy - path(p.z);
    q = rot(q, params.twist * sin(p.z * TAU * 5.0 / LOOP) + params.spin);

    let sector = TAU / sides;
    let angle = atan2(q.y, q.x);
    let a = wrap(angle + sector * 0.5, sector) - sector * 0.5;
    let r = length(q);
    let u = r * cos(a);
    let w = r * sin(a);

    let zc = wrap(p.z, CELL) - 0.5 * CELL;
    s.ring_id = wrap(floor(p.z / CELL), CELLS);
    s.along = w;
    s.z_cell = zc;
    s.sector = floor((angle + sector * 0.5) / sector);

    s.wall = RADIUS - u;

    let rib = abs(vec2f(u - (RADIUS - 0.12), zc)) - vec2f(0.12, 0.28);
    s.rib = length(max(rib, vec2f(0.0))) + min(max(rib.x, rib.y), 0.0);

    let edge = RADIUS * tan(sector * 0.5);
    s.rail = length(vec2f(u - (RADIUS - 0.1), abs(w) - edge)) - 0.06;

    s.ring_light = length(vec2f(u - (RADIUS - 0.30), zc)) - 0.04;

    // A square gate halfway between every other pair of rings, each at its own angle.
    let gz = wrap(p.z, 2.0 * CELL) - CELL;
    s.gate_id = wrap(floor(p.z / (2.0 * CELL)), CELLS * 0.5);
    let dir = select(-1.0, 1.0, wrap(s.gate_id, 2.0) < 1.0);
    let g = rot(q, s.gate_id * 0.7 + anim * 0.35 * dir);
    let square = max(abs(g.x), abs(g.y));
    let frame = abs(vec2f(square - gate_open - 0.13, gz)) - vec2f(0.13, 0.11);
    s.gate = length(max(frame, vec2f(0.0))) + min(max(frame.x, frame.y), 0.0);
    s.gate_light = length(vec2f(square - gate_open, gz)) - 0.035;
    return s;
}

fn solid(s: Parts) -> f32 {
    return min(min(s.wall, s.rib), min(s.rail, s.gate));
}

fn light(s: Parts) -> f32 {
    return min(s.ring_light, s.gate_light);
}

fn scene(p: vec3f) -> f32 {
    let s = parts(p);
    return min(solid(s), light(s));
}

fn normal_at(p: vec3f, e: f32) -> vec3f {
    let k = vec2f(1.0, -1.0);
    return normalize(
        k.xyy * scene(p + k.xyy * e) + k.yyx * scene(p + k.yyx * e)
        + k.yxy * scene(p + k.yxy * e) + k.xxx * scene(p + k.xxx * e)
    );
}

fn pick(t: f32) -> vec3f {
    if params.colours.count > 0 {
        return pn_colormap(params.colours, fract(t)).rgb;
    }
    return pow(0.5 + 0.5 * cos(TAU * (t + vec3f(0.0, 0.33, 0.67))), vec3f(2.2));
}

// A wave of light runs along the corridor, ring by ring.
fn chase(id: f32) -> f32 {
    let w = 0.5 + 0.5 * cos(TAU * (id / 8.0 - anim * 0.45));
    return 0.3 + 0.7 * w * w * w;
}

fn ring_colour(id: f32) -> vec3f {
    return pick(params.hue + id * 0.0833) * chase(id) * gain;
}

fn gate_colour(id: f32) -> vec3f {
    return pick(params.hue + 0.5 + id * 0.1667) * gain * 1.4;
}

fn near_colour(s: Parts) -> vec3f {
    if s.ring_light < s.gate_light {
        return ring_colour(s.ring_id);
    }
    return gate_colour(s.gate_id);
}

// What a mirror ray sees: the lights it passes close to, and the dark walls.
fn mirror(ro: vec3f, rd: vec3f) -> vec3f {
    var t = 0.05;
    var seen = vec3f(0.0);
    for (var i = 0; i < MIRROR_STEPS; i++) {
        let s = parts(ro + rd * t);
        let l = light(s);
        let d = min(solid(s), l) * step_scale;
        seen += near_colour(s) * exp(-9.0 * max(l, 0.0)) * max(d, 0.02);
        if d < 0.004 || t > 18.0 {
            break;
        }
        t += d;
    }
    return seen * 1.6;
}

fn shade(p: vec3f, rd: vec3f, t: f32, ro: vec3f, lamp: vec3f) -> vec3f {
    let s = parts(p);
    let lit = light(s);
    if lit <= solid(s) {
        return near_colour(s) * 2.6;
    }
    let n = normal_at(p, max(0.0015, 0.0008 * t));

    // Dark panels with a seam at the edge and, on some, a lit strip.
    var albedo = vec3f(0.018, 0.020, 0.028);
    var emit = vec3f(0.0);
    if s.wall <= min(s.rib, min(s.rail, s.gate)) {
        let seed = s.sector * 7.13 + s.ring_id * 3.77;
        albedo *= 0.6 + 1.3 * hash11(seed);
        let panel = 1.0 - smoothstep(0.018, 0.05, abs(abs(s.z_cell) - 0.95));
        albedo *= 1.0 + 3.0 * panel;
        if hash11(seed + 2.1) > 0.72 {
            let flicker = 0.5 + 0.5 * sin(anim * (1.5 + 5.0 * hash11(seed + 4.2)) + seed);
            let strip = (1.0 - smoothstep(0.02, 0.04, abs(s.along)))
                * (1.0 - smoothstep(0.55, 0.62, abs(s.z_cell)));
            emit = pick(params.hue + hash11(seed + 7.7)) * strip * flicker * gain * 1.8;
        }
    } else if s.gate <= min(s.rib, s.rail) {
        albedo = vec3f(0.035, 0.035, 0.045);
    } else {
        albedo = vec3f(0.05, 0.05, 0.06);
    }

    // The nearest light tints the surface, the camera's lamp picks out the shapes.
    let tint = near_colour(s) * (exp(-1.25 * max(lit, 0.0)) * 1.5 + 0.06);
    let to_lamp = lamp - p;
    let dist2 = dot(to_lamp, to_lamp);
    let l = to_lamp / sqrt(max(dist2, 1e-4));
    let lamp_light = max(dot(n, l), 0.0) / (1.0 + dist2 * 0.05);
    var c = albedo * (tint * 1.5 + vec3f(lamp_light) * 0.9 + 0.04) + emit;

    let h = normalize(l - rd);
    c += pow(max(dot(n, h), 0.0), 48.0) * lamp_light * 0.25;

    if params.gloss > 0.001 && t < 22.0 {
        let fres = pow(clamp(1.0 + dot(n, rd), 0.0, 1.0), 3.0);
        let reflected = mirror(p + n * 0.01, reflect(rd, n));
        c += reflected * params.gloss * (0.03 + 0.28 * fres);
    }
    return c;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    anim = pn_time.seconds;
    gain = 0.5 + 1.0 * clamp(params.energy, 0.0, 2.0) + 1.2 * clamp(params.flash, 0.0, 2.0);
    sides = f32(clamp(params.sides, 3, 16));
    gate_open = clamp(params.gate, 1.05, 1.9);
    // The path's slope stretches distances, shorter steps stay safe.
    step_scale = 0.9 / sqrt(1.0 + 0.09 * params.bend * params.bend);

    let aspect = pn.resolution.x / pn.resolution.y;
    let ndc = vec2f((v.uv.x - 0.5) * 2.0 * aspect, (0.5 - v.uv.y) * 2.0);

    let fly = wrap(pn_time.seconds * params.speed + params.travel, CELLS);
    let z = fly * CELL + params.camera.z;
    let ro = vec3f(path(z) + params.camera.xy, z);
    let ahead = z + max(params.aim.z, 0.5);
    let ta = vec3f(path(ahead) + params.aim.xy, ahead);

    let ww = normalize(ta - ro);
    // The camera banks into the path's curves.
    let side = path(z + 2.0) - path(z - 2.0);
    let bank = radians(params.roll) - side.x * 0.18;
    let up = vec3f(sin(bank), cos(bank), 0.0);
    let uu = normalize(cross(up, ww));
    let vv = cross(ww, uu);
    let focal = 1.0 / tan(radians(clamp(params.fov, 20.0, 150.0)) * 0.5);
    let rd = normalize(uu * ndc.x + vv * ndc.y + ww * focal);
    let lamp = ro + ww * 1.2;

    let pixel = 2.0 / (focal * pn.resolution.y);
    let steps = clamp(params.steps, 16, MAX_STEPS);
    var t = 0.02;
    var hit = false;
    var glow = vec3f(0.0);
    for (var i = 0; i < steps; i++) {
        let s = parts(ro + rd * t);
        let l = light(s);
        let d = min(solid(s), l) * step_scale;
        let near = exp(-7.0 * max(l, 0.0));
        if near > 0.01 {
            glow += near_colour(s) * near * max(d, 0.01);
        }
        if d < max(0.0012, pixel * t * 0.6) {
            hit = true;
            break;
        }
        t += d;
        if t > REACH {
            break;
        }
    }

    let haze = clamp(params.haze, 0.0, 1.0);
    let fog_colour = pick(params.hue + 0.2) * 0.015 * gain;
    var col = fog_colour;
    if hit {
        let surface = shade(ro + rd * t, rd, t, ro, lamp);
        let f = 1.0 - exp(-pow(t * (0.045 + 0.055 * haze), 2.0));
        col = mix(surface, fog_colour, f);
    }
    col += glow * 0.55;

    // Filmic curve: highlights roll off instead of clipping.
    col = max(col, vec3f(0.0));
    col = clamp((col * (2.51 * col + 0.03)) / (col * (2.43 * col + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));

    // Half a code value of noise in display space hides banding in the dark gradients.
    let n = fract(52.9829189 * fract(dot(v.pos.xy, vec2f(0.06711056, 0.00583715))));
    let shown = pow(col, vec3f(1.0 / 2.2)) + (n - 0.5) / 255.0;
    return vec4f(pow(max(shown, vec3f(0.0)), vec3f(2.2)), 1.0);
}
