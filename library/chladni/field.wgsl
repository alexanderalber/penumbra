// The plate's vibration: how far each point of the plate moves. Sand leaves where it moves and
// gathers where it stands still (the nodal lines).
// Output: r = amplitude, signed for the formula plates, about 1 at the strongest places;
// b = 1 on the plate, 0 off it; a = 1. Its size is the size of in0.
// in0: a picture of the field's size, in1: `pick` (the figure, 1 x 1), in2: the membrane state
// (shaped plate), in3: the plate's outline (the input shape, shaped plate).
//
// square: cos(n pi x) cos(m pi y) +- cos(m pi x) cos(n pi y) on x, y in 0..1, the classic
//   formula. It is exact for a square membrane with free edges; a real metal plate has slightly
//   different shapes, the same kind of figures.
// circle: J_n(k r) cos(n angle), k a zero of the slope of J_n (a free edge). Again a membrane,
//   not a metal plate: rings and spokes in the right numbers, not at the exact radii of a metal
//   plate.
// shape: the plate's outline is the picture in3; the figure comes from `membrane`, a wave
//   simulation driven at the figure's pitch: the part of its swing in step with the timing most
//   of the plate shares. That timing comes from the sums cc, ss, cs and count over the plate.
//
// @param plate choice(square|circle|shape) = square -- follows the component's plate
// @param size float = 0.9 0.2..1 -- square side or circle diameter, in picture heights
// @param turn float = 0 -180..180 -- turns the figure on the circular plate, in degrees
// @param cc float = 0 -- follows the sums: membrane C^2 over the plate
// @param ss float = 0 -- follows the sums: membrane S^2 over the plate
// @param cs float = 0 -- follows the sums: membrane C S over the plate
// @param count float = 1 -- follows the sums: membrane texels on the plate

const PI = 3.14159265;

// Figure k of the square plate: (m, n, sign), ordered by pitch m^2 + n^2.
fn sq_mode(k: i32) -> vec3f {
    var t = array<vec3f, 20>(
        vec3f(1.0, 0.0, -1.0), vec3f(2.0, 0.0, 1.0), vec3f(2.0, 1.0, -1.0), vec3f(2.0, 1.0, 1.0),
        vec3f(3.0, 0.0, -1.0), vec3f(3.0, 1.0, 1.0), vec3f(3.0, 2.0, -1.0), vec3f(4.0, 0.0, 1.0),
        vec3f(4.0, 1.0, -1.0), vec3f(4.0, 2.0, 1.0), vec3f(5.0, 0.0, -1.0), vec3f(4.0, 3.0, -1.0),
        vec3f(5.0, 2.0, 1.0), vec3f(5.0, 3.0, -1.0), vec3f(6.0, 1.0, 1.0), vec3f(6.0, 3.0, -1.0),
        vec3f(7.0, 2.0, 1.0), vec3f(7.0, 4.0, -1.0), vec3f(8.0, 3.0, 1.0), vec3f(9.0, 4.0, -1.0));
    return t[clamp(k, 0, 19)];
}

// Figure k of the circular plate: (n, k_ns, largest |J_n|), ordered by pitch k_ns.
fn circ_mode(k: i32) -> vec3f {
    var t = array<vec3f, 20>(
        vec3f(1.0, 1.8412, 0.5819), vec3f(2.0, 3.0542, 0.4865), vec3f(0.0, 3.8317, 1.0), vec3f(3.0, 4.2012, 0.4344),
        vec3f(4.0, 5.3176, 0.3997), vec3f(1.0, 5.3314, 0.5819), vec3f(5.0, 6.4156, 0.3741), vec3f(2.0, 6.7061, 0.4865),
        vec3f(0.0, 7.0156, 1.0), vec3f(6.0, 7.5013, 0.3541), vec3f(3.0, 8.0152, 0.4344), vec3f(1.0, 8.5363, 0.5819),
        vec3f(7.0, 8.5778, 0.3379), vec3f(4.0, 9.2824, 0.3997), vec3f(8.0, 9.6474, 0.3245), vec3f(2.0, 9.9695, 0.4865),
        vec3f(0.0, 10.1735, 1.0), vec3f(5.0, 10.5199, 0.3741), vec3f(3.0, 11.3459, 0.4344), vec3f(6.0, 11.7349, 0.3541));
    return t[clamp(k, 0, 19)];
}

// Bessel function J_n(x) = 1/pi * integral over 0..pi of cos(n t - x sin t): the midpoint rule
// with 32 points is exact to about 1e-6 for n and x below 16.
fn bessel(n: f32, x: f32) -> f32 {
    var s = 0.0;
    for (var i = 0; i < 32; i++) {
        let t = (f32(i) + 0.5) * PI / 32.0;
        s += cos(n * t - x * sin(t));
    }
    return s / 32.0;
}

fn square_u(q: vec2f, k: i32) -> f32 {
    let w = sq_mode(k);
    let a = cos(w.y * PI * q.x) * cos(w.x * PI * q.y);
    let b = cos(w.x * PI * q.x) * cos(w.y * PI * q.y);
    return 0.5 * (a + w.z * b);
}

fn circle_u(c: vec2f, k: i32) -> f32 {
    let w = circ_mode(k);
    let r = length(c);
    let ang = atan2(c.y, c.x);
    return bessel(w.x, w.y * min(r, 1.0)) * cos(w.x * ang) / w.z;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let f = textureLoad(in1, vec2i(0, 0), 0).r;
    let k0 = i32(floor(f));
    let t = f - floor(f);
    // Plate coordinates: c from -1 to 1 across the plate, q from 0 to 1.
    let aspect = pn.resolution.x / pn.resolution.y;
    var c = (v.uv - 0.5) * vec2f(aspect, 1.0) / (0.5 * params.size);
    var u = 0.0;
    var inside = 0.0;
    if params.plate == plate_square {
        let q = 0.5 * c + 0.5;
        inside = select(0.0, 1.0, all(abs(c) <= vec2f(1.0)));
        u = mix(square_u(q, k0), square_u(q, k0 + 1), t);
    } else if params.plate == plate_circle {
        let a = params.turn * PI / 180.0;
        c = vec2f(cos(a) * c.x + sin(a) * c.y, -sin(a) * c.x + cos(a) * c.y);
        inside = select(0.0, 1.0, length(c) <= 1.0);
        u = mix(circle_u(c, k0), circle_u(c, k0 + 1), t);
    } else {
        let m = textureSampleLevel(in3, pn_clamp, v.uv, 0.0);
        inside = select(0.0, 1.0, pn_luma(m.rgb) > 0.5);
        // Bilinear by hand over the membrane texels that are on the plate, so the edge does not
        // pull the field towards 0 (which would draw a false nodal line along the outline).
        let mn = vec2f(textureDimensions(in2));
        let g = v.uv * mn - 0.5;
        let g0 = floor(g);
        let fr = g - g0;
        var acc = vec3f(0.0);
        for (var j = 0; j < 4; j++) {
            let o = vec2i(j % 2, j / 2);
            let q = clamp(vec2i(g0) + o, vec2i(0), vec2i(mn) - 1);
            let s = textureLoad(in2, q, 0);
            let w = select(1.0 - fr.x, fr.x, o.x == 1) * select(1.0 - fr.y, fr.y, o.y == 1);
            if s.g < 15000.0 {
                acc += vec3f(w * s.b, w * s.a, w);
            }
        }
        let lock = acc.xy / max(acc.z, 1e-6);
        // The timing most of the plate swings with: the main axis of the cloud of (C, S).
        let phi = 0.5 * atan2(2.0 * params.cs, params.cc - params.ss);
        let swing = (params.cc + params.ss) / max(params.count, 1.0);
        u = 0.7 * dot(lock, vec2f(cos(phi), sin(phi))) / sqrt(max(swing, 1e-12));
    }
    return vec4f(u * inside, 0.0, inside, 1.0);
}
