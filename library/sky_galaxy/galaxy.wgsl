// A spiral galaxy seen at a slant, behind the city: a warm core, two arms of blue stars with
// pink knots of glowing gas, dark dust lanes along the inner edge of each arm, and a field of
// stars in front. All of it comes from hashes and noise, nothing from files. Black where the
// sky is empty, so it can be screened over another sky.
// @param spin float = 0 -360..360 -- how far the galaxy has turned, in degrees; bind it to the distance travelled
// @param arms int = 2 1..6 -- number of spiral arms
// @param twist float = 2.2 0..6 -- how tightly the arms wind
// @param tilt float = 64 0..85 -- how far the disc leans away from us, in degrees
// @param angle float = -20 -90..90 -- the disc's angle in the picture, in degrees
// @param center vec2 = [0.56, 0.44] -- where the core sits, 0..1 across and down
// @param size float = 0.8 0.1..2 -- the disc's radius, in picture heights
// @param bulge float = 0.09 0.02..0.5 -- the core's radius, as a share of the disc
// @param dust float = 0.8 0..2 -- how dark the dust lanes are
// @param energy float = 0 0..1 -- lifts the light of the disc and the core; bind it to the bass
// @param twinkle float = 0 0..1 -- how bright the stars flash; bind it to a beat that fades
// @param warmth float = 0 0..1 -- 0 cool blue arms, 1 warm golden ones
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the galaxy is there: 1 all of it, 0 nothing (a dark sky)

fn rot(p: vec2f, a: f32) -> vec2f {
    let c = cos(a);
    let s = sin(a);
    return vec2f(c * p.x - s * p.y, s * p.x + c * p.y);
}

fn hash2(p: vec2f, seed: u32) -> f32 {
    let i = vec2i(floor(p));
    return f32(pn_hash(vec3u(bitcast<vec2u>(i), seed)).x) / 4294967296.0;
}

fn noise(p: vec2f, seed: u32) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash2(i, seed);
    let b = hash2(i + vec2f(1.0, 0.0), seed);
    let c = hash2(i + vec2f(0.0, 1.0), seed);
    let d = hash2(i + vec2f(1.0, 1.0), seed);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2f, seed: u32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    for (var k = 0u; k < 5u; k++) {
        sum += amp * noise(q, seed + k * 17u);
        q = rot(q, 0.6) * 2.03;
        amp *= 0.5;
    }
    return sum;
}

// Stars on a grid of cells, one per cell at a random place, most of them faint.
fn star_field(px: vec2f, cell: f32, seed: u32, flash: f32) -> f32 {
    let g = px / cell;
    let id = floor(g);
    let h = vec3f(pn_hash(vec3u(bitcast<vec2u>(vec2i(id)), seed))) / 4294967296.0;
    let at = id + 0.15 + 0.7 * h.xy;
    let d = length(g - at) * cell;
    let bright = pow(h.z, 16.0);
    // Some stars answer the beat, each a little differently.
    let answer = 1.0 + flash * 3.0 * step(0.6, fract(h.x * 31.0 + h.y * 7.0));
    return bright * answer * exp(-d * d * 0.9);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let res = pn.resolution;
    let px = v.pos.xy;
    // Into the disc's plane: centred, turned upright, stretched against the tilt.
    var p = (px - params.center * res) / res.y;
    p = rot(p, -radians(params.angle));
    p.y /= max(cos(radians(params.tilt)), 0.05);
    let q = p / params.size;
    let r = length(q);
    let theta = atan2(q.y, q.x);
    let spin = radians(params.spin);
    // The disc's features turn with it.
    let qs = rot(q, -spin);

    // Arms: a logarithmic spiral, made ragged by noise.
    let ragged = 1.2 * (fbm(qs * 3.0, 3u) - 0.5);
    let s = f32(params.arms) * (theta - spin + params.twist * log(r + 0.02)) + ragged;
    let arm = pow(0.5 + 0.5 * cos(s), 3.0);
    let disc = exp(-r * 3.0) * smoothstep(1.25, 0.6, r);
    let lift = 0.85 + 0.4 * params.energy;

    // Blue stars along the arms, clumped.
    let clumps = fbm(qs * 9.0, 41u);
    let stars_in_arm = arm * disc * (0.5 + 1.6 * clumps);
    // Pink knots of gas where the arms are densest.
    let knots = pow(max(fbm(qs * 16.0, 77u) - 0.45, 0.0) * 3.0, 3.0) * arm * disc;
    // Dust lanes on the inner edge of each arm.
    let lane = pow(0.5 + 0.5 * cos(s - 0.7), 6.0) * smoothstep(0.08, 0.3, r) * smoothstep(1.0, 0.5, r);
    let absorb = exp(-params.dust * 2.5 * lane * (0.4 + fbm(qs * 7.0, 99u)));
    // The core and a faint halo.
    let core = exp(-pow(r / params.bulge, 2.0)) * 1.6 + exp(-r / (params.bulge * 0.35)) * 0.8;
    let halo = exp(-r * 1.4) * 0.12;

    let cool = vec3f(0.45, 0.62, 1.0);
    let gold = vec3f(1.0, 0.72, 0.42);
    let arm_colour = mix(cool, gold, params.warmth);
    let core_colour = mix(vec3f(1.0, 0.88, 0.7), vec3f(1.0, 0.75, 0.5), params.warmth);
    var light = arm_colour * stars_in_arm * 1.5 + vec3f(1.0, 0.35, 0.55) * knots * 1.4;
    light += mix(arm_colour, core_colour, 0.6) * halo;
    light *= absorb;
    light += core_colour * core * (0.4 + 0.25 * params.energy);
    light *= lift;

    // Stars in front: many faint ones, a few bright.
    var field = star_field(px, 3.0, 5u, params.twinkle) * 0.1 + star_field(px, 11.0, 9u, params.twinkle) * 0.9;
    field += star_field(px, 47.0, 13u, params.twinkle) * 2.5;
    light += vec3f(0.85, 0.9, 1.0) * field;

    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
