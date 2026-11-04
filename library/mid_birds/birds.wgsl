// The flock of mid_birds, one texel per bird, for two geo.tex_instance nodes: one with the birds'
// bodies (body.obj), one with their wings (wings.obj; birds.py writes both: +y forward, z up, the
// wings spread along x, 1 m across). Now and then a flock crosses the
// view far ahead, low over the horizon, so the birds stand black against the band of first
// light. A crossing takes `cross` beats of travel; one begins every `every` beats, from the left
// or from the right. The birds fly in a loose, wavering skein, each beating its wings once a
// beat in its own phase and gliding now and then. The flapping is the wings' height scaled from
// tips up to tips down.
// The flock keeps its height above the horizon as the camera rises and falls: `low` and `high`
// are in picture heights of a 60 degree view, as the stage's sky counts them.
// `what` place: the bird's middle (rgb). aim: its heading. scale: the body's size (0 for none).
// wings: the wings' size, z times the wingbeat. Render it 8 x 4.
// @param what choice(place|aim|scale|wings) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param eye float = 1.2 0..10 -- the camera's height over the water, metres (height of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 40 1..200 -- metres ahead where the horizon lies (far of the stage's pose)
// @param presence float = 1 0..1 -- how many birds fly: towards 0 fewer, 0 none
// @param birds int = 24 1..32 -- birds in a flock
// @param size float = 0.9 0.2..3 -- a bird's wingspan, metres
// @param at float = 17 6..40 -- metres ahead where the flock crosses
// @param low float = 0.012 -0.05..0.5 -- the lowest a flock flies, picture heights above the horizon
// @param high float = 0.045 -0.05..0.5 -- the highest
// @param every float = 64 8..1000 -- beats from one crossing to the next
// @param cross float = 48 4..1000 -- beats a crossing takes
// @param gap float = 1 0.3..4 -- metres between two birds of the skein
// @param flap float = 1 0..4 -- wingbeats per beat
// @param phase float = 0 0..1000 -- beats added to the travelled ones for the crossings: moves the first flock earlier
// @param seed int = 0 0..1000 -- other flocks

const TAU: f32 = 6.2831853;

fn rand(n: i32, k: u32, salt: u32) -> vec3f {
    let s = u32(max(params.seed, 0));
    return vec3f(pn_hash(vec3u(bitcast<u32>(n), k + 31u * s, salt))) / 4294967296.0;
}

struct Bird {
    at: vec3f,
    heading: vec3f,
    size: f32,
    wings: f32,
}

fn bird(k: u32) -> Bird {
    var b: Bird;
    b.at = vec3f(0.0, 10.0, 0.0);
    b.heading = vec3f(1.0, 0.0, 0.0);
    b.size = 0.0;
    b.wings = 0.0;
    let count = u32(clamp(params.birds, 1, 32));
    let every = max(params.every, 1.0);
    let cross = clamp(params.cross, 1.0, every);
    let time = params.beats + max(params.phase, 0.0);
    let crossing = floor(time / every);
    let t = (time - crossing * every) / cross;
    let shown = clamp(params.presence, 0.0, 1.0) * f32(count);
    if k >= count || t >= 1.0 || f32(k) >= ceil(shown) || params.presence < 0.001 {
        return b;
    }
    let n = i32(crossing);
    let p = rand(n, 3u, 11u);
    let dir = select(-1.0, 1.0, p.x < 0.5);
    // The flock's path: straight across, a little towards or away from the camera, rising or
    // sinking a little on the way.
    let half = 1.35 * params.at + params.gap * f32(count) * 0.5;
    let lean = (p.y - 0.5) * 0.25;
    let x0 = dir * mix(-half, half, t);
    let y0 = params.at + lean * x0 * dir;
    let level = mix(params.low, params.high, p.z) + 0.25 * (params.high - params.low) * sin(3.1416 * t) * (p.y - 0.5);

    // The skein: two wavering arms behind the leader, the birds in turns on either arm.
    let r = rand(n, k, 13u);
    let rank = f32((k + 1u) / 2u);
    let arm = select(-1.0, 1.0, k % 2u == 0u);
    let wander = vec3f(pn_noise(vec3f(f32(k) * 1.7, params.beats * 0.06, 0.5), 5u),
                       pn_noise(vec3f(f32(k) * 1.7, params.beats * 0.06, 2.5), 5u),
                       pn_noise(vec3f(f32(k) * 1.7, params.beats * 0.06, 4.5), 5u));
    let g = params.gap;
    let back = g * rank * (0.9 + 0.3 * r.x);
    let off = vec3f(-dir * back, arm * g * rank * 0.2, 0.0) + g * vec3f(0.4, 0.4, 0.0) * wander;
    // The arms climb a little away from the leader, in picture heights, so the whole skein stays
    // in the band however deep it reaches.
    let rise = arm * rank * 0.0012 + 0.004 * wander.z;

    let y = y0 + off.y;
    // Picture heights above the horizon to metres above the water at y metres ahead.
    let unit = 2.0 * tan(radians(30.0));
    let eye = max(params.eye, 0.0);
    let z = eye + y * ((level + rise) * unit - eye / max(params.reach, 1.0));
    let x = x0 + off.x + y * tan(radians(params.side));
    b.at = vec3f(x, y, z);
    b.heading = normalize(vec3f(dir, lean, 0.04 * wander.z));
    // Wingbeats: once a beat each, in its own phase; now and then a bird glides.
    let glide = smoothstep(0.1, 0.5, pn_noise(vec3f(f32(k) * 3.1, params.beats / 12.0, 7.0), 9u) + 0.3);
    let beat = sin(TAU * (params.beats * params.flap + r.y));
    let wings = 0.3 + mix(0.15, 1.0, glide) * 0.95 * beat;
    let last = clamp(shown - f32(k), 0.0, 1.0);
    b.size = params.size * (0.85 + 0.3 * r.z) * last;
    b.wings = wings;
    return b;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let b = bird(cell.y * 8u + cell.x);
    if params.what == what_aim {
        return vec4f(b.heading, 1.0);
    }
    if params.what == what_scale {
        return vec4f(vec3f(b.size), 1.0);
    }
    if params.what == what_wings {
        return vec4f(b.size, b.size, b.size * b.wings, 1.0);
    }
    return vec4f(b.at, 1.0);
}
