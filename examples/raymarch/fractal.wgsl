// A Mandelbulb: a 3D fractal made visible by marching along its distance estimate. Its form
// wanders on its own. It animates with nothing wired in.
// @param energy float = 0.35 0..1 -- brightness of the glow, and how far the form trembles. Good for the music's level.
// @param kick float = 0 0..1 -- a bright flash in the fractal's body. Good for a beat.
// @param morph float = 0 -1..1 -- turns the fractal's bulbs. Good for a smoothed level.
// @param hue float = 0 0..1 -- moves the surface along the palette.
// @param colours palette = plasma -- colours of the surface. With no palette, a built-in rainbow.
// @param speed float = 1 0..4 -- speed of the wandering. Changing it makes the form jump, so set it once and bind `phase` instead.
// @param phase float = 0 0..1000 -- seconds added to the wandering. Bind a channel that only grows.
// @group form
// @param detail int = 8 4..14 -- detail of the fractal. Each one costs more on every step.
// @param power float = 8 3..12 -- the bulbs' symmetry: 8 is the classic form, lower is rounder. Whole numbers have no seam; between them a line can show on the surface.
// @param julia float = 0 0..1 -- 0 is the plain Mandelbulb, 1 a Julia form set by `seed`.
// @param seed vec3 = [0.4, -0.6, 0.5] -- the Julia form's shape, used when `julia` is above 0.
// @group camera
// @param camera vec3 = [0, 0.9, 3.4] -- camera position at the start of its orbit.
// @param aim vec3 = [0, 0, 0] -- the point the camera looks at, and the centre of its orbit.
// @param orbit float = 0.1 -1..1 -- orbit speed in radians per second. Changing it makes the camera jump, so set it once and bind `orbit_angle` instead.
// @param orbit_angle float = 0 -180..180 -- turns the camera around `aim`, in degrees.
// @param fov float = 40 15..100 -- vertical field of view in degrees.
// @group look
// @param steps int = 88 24..200 -- most march steps per pixel. More shows finer detail at higher cost.
// @param shadow float = 0.6 0..1 -- depth of the soft shadows. 0 turns them off and costs less.
// @param glow float = 0.5 0..1 -- halo around the fractal.
// @param background color = [0.006, 0.006, 0.015, 1] -- colour of the empty space around the fractal.

const TAU: f32 = 6.2831853;
const MAX_STEPS: i32 = 220;      // upper limit whatever `steps` says, for a bounded cost
const MAX_ITERATIONS: i32 = 16;  // upper limit whatever `detail` says
const SHADOW_STEPS: i32 = 16;
const BOUND: f32 = 2.2;          // the fractal stays inside this radius for powers from 3 up

// Values that hold for the whole pixel, worked out once in `main`.
var<private> iterations: i32;
var<private> shape_power: f32;
var<private> theta_shift: f32;
var<private> phi_shift: f32;
var<private> julia: f32;
var<private> julia_seed: vec3f;

fn pick(t: f32) -> vec3f {
    if params.colours.count > 0 {
        return pn_colormap(params.colours, clamp(t, 0.0, 1.0)).rgb;
    }
    return pow(0.5 + 0.5 * cos(TAU * (t + vec3f(0.0, 0.33, 0.67))), vec3f(2.2));
}

fn rot_y(a: f32) -> mat3x3f {
    let c = cos(a);
    let s = sin(a);
    return mat3x3f(vec3f(c, 0.0, -s), vec3f(0.0, 1.0, 0.0), vec3f(s, 0.0, c));
}

// Distance estimate in x, the orbit's closest approach to the origin in y (0..1).
fn bulb(p: vec3f, count: i32) -> vec2f {
    var z = p;
    let c = mix(p, julia_seed, julia);
    var dr = 1.0;
    var r = length(z);
    var trap = 1.0;
    for (var i = 0; i < count; i++) {
        if r > 4.0 || r < 1e-5 {
            break;
        }
        let powered = pow(r, shape_power - 1.0);
        dr = min(powered * shape_power * dr + 1.0, 1e24);
        let theta = acos(clamp(z.z / r, -1.0, 1.0)) * shape_power + theta_shift;
        let phi = atan2(z.y, z.x) * shape_power + phi_shift;
        let zr = powered * r;
        z = zr * vec3f(sin(theta) * cos(phi), sin(phi) * sin(theta), cos(theta)) + c;
        r = length(z);
        trap = min(trap, r);
    }
    let safe_r = max(r, 1e-5);
    return vec2f(0.5 * log(safe_r) * safe_r / dr, trap);
}

fn dist(p: vec3f) -> f32 {
    return bulb(p, iterations).x;
}

fn normal_at(p: vec3f, e: f32) -> vec3f {
    let k = vec2f(1.0, -1.0);
    return normalize(
        k.xyy * dist(p + k.xyy * e) + k.yyx * dist(p + k.yyx * e)
        + k.yxy * dist(p + k.yxy * e) + k.xxx * dist(p + k.xxx * e)
    );
}

fn shadow_at(p: vec3f, l: vec3f, start: f32) -> f32 {
    var res = 1.0;
    var t = start;
    let cheap = max(iterations - 3, 3);
    for (var i = 0; i < SHADOW_STEPS; i++) {
        let d = bulb(p + l * t, cheap).x;
        res = min(res, 10.0 * d / t);
        if res < 0.02 || t > 2.5 {
            break;
        }
        t += clamp(d, start, 0.4);
    }
    return clamp(res, 0.0, 1.0);
}

// Where a ray meets the sphere that holds the fractal: entry and exit distance, x > y when it misses.
fn bound_hit(ro: vec3f, rd: vec3f) -> vec2f {
    let b = dot(ro, rd);
    let c = dot(ro, ro) - BOUND * BOUND;
    let h = b * b - c;
    if h < 0.0 {
        return vec2f(1.0, 0.0);
    }
    let root = sqrt(h);
    return vec2f(max(-b - root, 0.0), -b + root);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let t = pn_time.seconds * params.speed + params.phase;
    let energy = clamp(params.energy, 0.0, 2.0);
    let kick = clamp(params.kick, 0.0, 2.0);

    iterations = clamp(params.detail, 4, MAX_ITERATIONS);
    // The form wanders slowly and trembles a little with the energy. Only the shifts move:
    // an animated power would leave whole numbers and show a seam on the surface.
    shape_power = clamp(params.power, 3.0, 12.0);
    theta_shift = params.morph * 3.14159 + 0.35 * sin(t * 0.21) + 0.08 * energy * sin(t * 2.3);
    phi_shift = t * 0.11 + 0.6 * sin(t * 0.17);
    julia = clamp(params.julia, 0.0, 1.0);
    julia_seed = params.seed;

    let aspect = pn.resolution.x / pn.resolution.y;
    let ndc = vec2f((v.uv.x - 0.5) * 2.0 * aspect, (0.5 - v.uv.y) * 2.0);

    let angle = t * clamp(params.orbit, -4.0, 4.0) + radians(params.orbit_angle);
    let ro = params.aim + rot_y(angle) * (params.camera - params.aim);
    let ww = normalize(params.aim - ro);
    let uu = normalize(cross(vec3f(0.0, 1.0, 0.0), ww));
    let vv = cross(ww, uu);
    let focal = 1.0 / tan(radians(clamp(params.fov, 10.0, 150.0)) * 0.5);
    let rd = normalize(uu * ndc.x + vv * ndc.y + ww * focal);
    let pixel = 2.0 / (focal * pn.resolution.y);

    var col = params.background.rgb * (1.0 - 0.5 * dot(ndc, ndc) * 0.5);
    let span = bound_hit(ro, rd);

    if span.x < span.y {
        let steps = clamp(params.steps, 24, MAX_STEPS);
        var along = span.x;
        var hit = false;
        var halo = 0.0;
        var used = 0.0;
        var trap = 1.0;
        for (var i = 0; i < steps; i++) {
            let s = bulb(ro + rd * along, iterations);
            let d = s.x * 0.9;
            trap = s.y;
            used += 1.0;
            halo += exp(-max(d, 0.0) * 22.0) * max(d, 0.002);
            if d < max(0.0004, pixel * along * 0.7) {
                hit = true;
                break;
            }
            along += d;
            if along > span.y {
                break;
            }
        }

        let glow_amount = clamp(params.glow, 0.0, 1.0);
        let palette_shift = params.hue;
        let halo_colour = pick(fract(palette_shift + 0.75));
        col += halo_colour * halo * glow_amount * (0.5 + 3.0 * energy);

        if hit {
            let p = ro + rd * along;
            let n = normal_at(p, max(0.0006, along * pixel));
            let l = normalize(vec3f(0.55, 0.75, -0.35));
            var shade = 1.0;
            if params.shadow > 0.001 {
                let start = max(0.004, along * pixel * 2.0);
                shade = mix(1.0, shadow_at(p + n * start, l, start), clamp(params.shadow, 0.0, 1.0));
            }
            let occl = pow(clamp(1.0 - used / f32(steps) * 1.15, 0.0, 1.0), 2.0);
            // A triangle wave along the palette: no hard edge where the bands repeat.
            let bands = 1.0 - abs(2.0 * fract(palette_shift + sqrt(clamp(trap, 0.0, 1.0)) * 1.1) - 1.0);
            let albedo = pick(bands);
            let diff = max(dot(n, l), 0.0) * shade;
            let view = -rd;
            let fres = pow(1.0 - max(dot(n, view), 0.0), 4.0);
            let h = normalize(l + view);
            let spec = pow(max(dot(n, h), 0.0), 40.0) * shade;

            var surface = albedo * (diff * 1.5 + 0.16 * occl + 0.25 * occl * (0.5 + 0.5 * n.y));
            surface += spec * 0.7;
            surface += pick(fract(palette_shift + 0.6)) * fres * occl * (0.5 + 2.0 * energy);
            // A beat lights the deep parts of the fractal from inside.
            surface += albedo * kick * 3.0 * (1.0 - occl) * (1.0 - occl);
            col = surface * (0.75 + 0.9 * energy);
        }
    }

    col = max(col, vec3f(0.0));
    col = clamp((col * (2.51 * col + 0.03)) / (col * (2.43 * col + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));

    // Half a code value of noise in display space hides banding in the dark gradients.
    let n2 = fract(52.9829189 * fract(dot(v.pos.xy, vec2f(0.06711056, 0.00583715))));
    let shown = pow(col, vec3f(1.0 / 2.2)) + (n2 - 0.5) / 255.0;
    return vec4f(pow(max(shown, vec3f(0.0)), vec3f(2.2)), 1.0);
}
