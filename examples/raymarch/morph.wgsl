// Several glossy shapes drift around a centre and melt into each other. Where two shapes
// touch they blend, in form and in colour. Each shape slowly changes between sphere, box,
// ring, diamond and capsule. It animates with nothing wired in.
// @param energy float = 0.35 0..1 -- swells the shapes and brightens the glow. Good for the music's level.
// @param kick float = 0 0..1 -- pushes the shapes outward for a moment. Good for a beat.
// @param blend float = 0.65 0.05..1.5 -- how softly the shapes melt into each other.
// @param hue float = 0 0..1 -- moves the shapes along the palette.
// @param colours palette = ff2e63 08d9d6 ff9a00 7c3aed -- colours of the shapes. With no palette, a built-in rainbow.
// @param speed float = 1 0..4 -- speed of the drift. Changing it makes the shapes jump, so set it once and bind `phase` instead.
// @param phase float = 0 0..1000 -- seconds added to the drift. Bind a channel that only grows.
// @group shape
// @param shapes int = 5 2..6 -- number of shapes.
// @param spread float = 1.5 0..2 -- how far the shapes move from the centre.
// @param form float = 0.7 0..1 -- 0 keeps every shape a round ball, 1 shows the full box, ring and diamond.
// @param ripple float = 0.2 0..1 -- waves on the surface.
// @group camera
// @param camera vec3 = [0, 0.7, 6.2] -- camera position at the start of its orbit.
// @param aim vec3 = [0, 0, 0] -- the point the camera looks at, and the centre of its orbit.
// @param orbit float = 0.15 -1..1 -- orbit speed in radians per second. Changing it makes the camera jump, so set it once and bind `orbit_angle` instead.
// @param orbit_angle float = 0 -180..180 -- turns the camera around `aim`, in degrees.
// @param fov float = 42 15..100 -- vertical field of view in degrees.
// @group look
// @param steps int = 72 16..160 -- most march steps per pixel. More sharpens the edges at higher cost.
// @param shadow float = 0.7 0..1 -- depth of the soft shadows. 0 turns them off and costs less.
// @param background color = [0.012, 0.014, 0.03, 1] -- colour of the empty space around the shapes.

const TAU: f32 = 6.2831853;
const SLOTS: i32 = 6;
const MAX_STEPS: i32 = 200;   // upper limit whatever `steps` says, for a bounded cost
const SHADOW_STEPS: i32 = 18;
const REACH: f32 = 22.0;

// Values that hold for the whole pixel, worked out once in `main`.
var<private> time_now: f32;
var<private> slot_count: i32;
var<private> slot_pos: array<vec3f, 6>;
var<private> slot_axes: array<mat3x3f, 6>;
var<private> slot_colour: array<vec3f, 6>;
var<private> slot_radius: array<f32, 6>;
var<private> slot_kind_a: array<i32, 6>;
var<private> slot_kind_b: array<i32, 6>;
var<private> slot_kind_mix: array<f32, 6>;
var<private> soft: f32;
var<private> form_amount: f32;
var<private> ripple_size: f32;
var<private> rim_a: vec3f;
var<private> rim_b: vec3f;
var<private> rim_c: vec3f;

fn pick(t: f32) -> vec3f {
    let n = params.colours.count;
    if n > 0 {
        // Reads the palette directly: pn_colormap copies all 32 colours per call.
        let x = fract(t) * f32(n - 1);
        let i = clamp(i32(x), 0, n - 1);
        let j = min(i + 1, n - 1);
        return mix(params.colours.colours[i].rgb, params.colours.colours[j].rgb, x - f32(i));
    }
    return pow(0.5 + 0.5 * cos(TAU * (t + vec3f(0.0, 0.33, 0.67))), vec3f(2.2));
}

fn rot_y(a: f32) -> mat3x3f {
    let c = cos(a);
    let s = sin(a);
    return mat3x3f(vec3f(c, 0.0, -s), vec3f(0.0, 1.0, 0.0), vec3f(s, 0.0, c));
}

fn rot_x(a: f32) -> mat3x3f {
    let c = cos(a);
    let s = sin(a);
    return mat3x3f(vec3f(1.0, 0.0, 0.0), vec3f(0.0, c, s), vec3f(0.0, -s, c));
}

fn shape(kind: i32, q: vec3f, r: f32) -> f32 {
    var d = 0.0;
    switch kind {
        case 0: {
            d = length(q) - r;
        }
        case 1: {
            let b = abs(q) - vec3f(r * 0.72);
            d = length(max(b, vec3f(0.0))) + min(max(b.x, max(b.y, b.z)), 0.0) - r * 0.14;
        }
        case 2: {
            d = length(vec2f(length(q.xz) - r * 0.85, q.y)) - r * 0.32;
        }
        case 3: {
            let a = abs(q);
            d = (a.x + a.y + a.z - r * 1.5) * 0.57735;
        }
        default: {
            let y = q.y - clamp(q.y, -r * 0.9, r * 0.9);
            d = length(vec3f(q.x, y, q.z)) - r * 0.5;
        }
    }
    return d;
}

// Distance in x, and in rgb the colour of the shapes blended the same way as their surfaces.
fn scene(p: vec3f) -> vec4f {
    var d = 1000.0;
    var col = vec3f(0.0);
    for (var i = 0; i < slot_count; i++) {
        let q = slot_axes[i] * (p - slot_pos[i]);
        let r = slot_radius[i];
        let ball = length(q) - r;
        let solid = mix(shape(slot_kind_a[i], q, r), shape(slot_kind_b[i], q, r), slot_kind_mix[i]);
        let e = mix(ball, solid, form_amount);
        if i == 0 {
            d = e;
            col = slot_colour[i];
        } else {
            let h = clamp(0.5 + 0.5 * (d - e) / soft, 0.0, 1.0);
            d = mix(d, e, h) - soft * h * (1.0 - h);
            // A narrower blend than the surface's keeps the colours pure away from the seam.
            col = mix(col, slot_colour[i], smoothstep(0.25, 0.75, h));
        }
    }
    if ripple_size > 0.0005 {
        let w = sin(p.x * 3.1 + time_now * 1.1) * sin(p.y * 2.7 - time_now * 0.8) * sin(p.z * 3.4 + time_now * 0.6);
        d += ripple_size * w;
    }
    return vec4f(d, col);
}

fn dist(p: vec3f) -> f32 {
    return scene(p).x;
}

fn normal_at(p: vec3f, e: f32) -> vec3f {
    let k = vec2f(1.0, -1.0);
    return normalize(
        k.xyy * dist(p + k.xyy * e) + k.yyx * dist(p + k.yyx * e)
        + k.yxy * dist(p + k.yxy * e) + k.xxx * dist(p + k.xxx * e)
    );
}

fn shadow_at(p: vec3f, l: vec3f) -> f32 {
    var res = 1.0;
    var t = 0.04;
    for (var i = 0; i < SHADOW_STEPS; i++) {
        let d = dist(p + l * t);
        res = min(res, 8.0 * d / t);
        if res < 0.02 || t > 8.0 {
            break;
        }
        t += clamp(d, 0.03, 0.8);
    }
    return clamp(res, 0.0, 1.0);
}

fn occlusion(p: vec3f, n: vec3f) -> f32 {
    var a = 0.0;
    var w = 1.0;
    for (var i = 1; i <= 4; i++) {
        let h = 0.05 + 0.13 * f32(i);
        a += w * max(h - dist(p + n * h), 0.0);
        w *= 0.6;
    }
    return clamp(1.0 - 1.6 * a, 0.0, 1.0);
}

// Light from all around: a dark room with three coloured soft lights in it.
fn room(rd: vec3f) -> vec3f {
    var c = mix(params.background.rgb * 0.5, params.background.rgb * 2.2, 0.5 + 0.5 * rd.y);
    c += rim_a * pow(max(dot(rd, normalize(vec3f(0.7, 0.6, 0.4))), 0.0), 16.0) * 2.2;
    c += rim_b * pow(max(dot(rd, normalize(vec3f(-0.8, 0.2, -0.5))), 0.0), 10.0) * 1.6;
    c += rim_c * pow(max(dot(rd, normalize(vec3f(0.1, -0.9, 0.3))), 0.0), 6.0) * 1.2;
    return c;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let t = pn_time.seconds * params.speed + params.phase;
    time_now = t;
    slot_count = clamp(params.shapes, 2, SLOTS);
    soft = max(params.blend, 0.02);
    form_amount = clamp(params.form, 0.0, 1.0);
    ripple_size = clamp(params.ripple, 0.0, 1.0) * 0.09;
    let energy = clamp(params.energy, 0.0, 2.0);
    let kick = clamp(params.kick, 0.0, 2.0);
    let spread = clamp(params.spread, 0.0, 4.0) * (1.0 + 0.35 * kick);

    rim_a = pick(params.hue + 0.15);
    rim_b = pick(params.hue + 0.55);
    rim_c = pick(params.hue + 0.8);

    for (var i = 0; i < SLOTS; i++) {
        let f = f32(i);
        let a = t * (0.21 + 0.05 * f) + f * 2.399;
        let b = t * (0.17 + 0.03 * f) + f * 1.3;
        // Each shape swings in to the centre and out again, so the shapes keep meeting.
        let reach = spread * (0.5 + 0.5 * sin(t * (0.31 + 0.04 * f) + f * 1.9));
        slot_pos[i] = reach * vec3f(
            sin(a) * cos(b * 0.7),
            0.75 * sin(b + f),
            cos(a) * cos(b * 0.7),
        );
        slot_axes[i] = rot_y(t * 0.4 + f) * rot_x(t * 0.33 + 2.1 * f);
        slot_colour[i] = pick(params.hue + f / f32(slot_count));
        slot_radius[i] = (0.58 + 0.13 * sin(t * 0.7 + f * 1.7)) * (1.0 + 0.25 * energy);
        // Each shape holds its form, then changes to the next one over its last 30 %.
        let cycle = t / 7.0 + f * 0.37;
        let n = floor(cycle);
        slot_kind_a[i] = i32(n + f) % 5;
        slot_kind_b[i] = i32(n + f + 1.0) % 5;
        slot_kind_mix[i] = smoothstep(0.7, 1.0, cycle - n);
    }

    let aspect = pn.resolution.x / pn.resolution.y;
    let ndc = vec2f((v.uv.x - 0.5) * 2.0 * aspect, (0.5 - v.uv.y) * 2.0);

    let angle = t * clamp(params.orbit, -4.0, 4.0) + radians(params.orbit_angle);
    let offset = rot_y(angle) * (params.camera - params.aim);
    let ro = params.aim + offset;
    let ww = normalize(params.aim - ro);
    let uu = normalize(cross(vec3f(0.0, 1.0, 0.0), ww));
    let vv = cross(ww, uu);
    let focal = 1.0 / tan(radians(clamp(params.fov, 10.0, 150.0)) * 0.5);
    let rd = normalize(uu * ndc.x + vv * ndc.y + ww * focal);
    let pixel = 2.0 / (focal * pn.resolution.y);

    let steps = clamp(params.steps, 16, MAX_STEPS);
    var dist_along = 0.0;
    var hit = false;
    var aura = vec3f(0.0);
    var surface_colour = vec3f(0.0);
    // Offset surfaces and blends stretch the field a little, shorter steps stay safe.
    let step_scale = 0.8;
    for (var i = 0; i < steps; i++) {
        let s = scene(ro + rd * dist_along);
        let d = s.x;
        // Light gathered along the ray, strongest close to the shapes.
        aura += s.yzw * exp(-max(d, 0.0) * 5.0) * max(d, 0.002);
        if d < max(0.0008, pixel * dist_along * 0.7) {
            hit = true;
            surface_colour = s.yzw;
            break;
        }
        dist_along += d * step_scale;
        if dist_along > REACH {
            break;
        }
    }

    let vignette = 1.0 - 0.45 * dot(ndc * 0.5, ndc * 0.5);
    var col = room(rd * vec3f(1.0, 1.0, 0.2)) * 0.25 * vignette;
    // The glow is brightest close around the shapes.
    col += aura * (0.05 + 0.32 * energy);
    if hit {
        let p = ro + rd * dist_along;
        let n = normal_at(p, max(0.0015, dist_along * pixel));
        let l = normalize(vec3f(0.6, 0.8, 0.5));
        var shadow_amount = 1.0;
        if params.shadow > 0.001 {
            shadow_amount = mix(1.0, shadow_at(p + n * 0.02, l), clamp(params.shadow, 0.0, 1.0));
        }
        let ao = occlusion(p, n);
        let diff = max(dot(n, l), 0.0) * shadow_amount;
        let view = -rd;
        let fres = pow(1.0 - max(dot(n, view), 0.0), 4.0);
        let refl = room(reflect(rd, n));
        // A glossy paint: the colour under a clear coat that mirrors the room.
        let albedo = pow(surface_colour, vec3f(1.2));
        col = albedo * (diff * 0.85 + ao * 0.3 + 0.04);
        col += refl * ao * (0.1 + 0.75 * fres);
        let h = normalize(l + view);
        col += pow(max(dot(n, h), 0.0), 90.0) * shadow_amount * 0.8;
        col += albedo * fres * (0.2 + 1.1 * energy);
        col *= 0.65 + 0.6 * energy;
    }

    col = max(col, vec3f(0.0));
    col = clamp((col * (2.51 * col + 0.03)) / (col * (2.43 * col + 0.59) + 0.14), vec3f(0.0), vec3f(1.0));

    // Half a code value of noise in display space hides banding in the dark gradients.
    let n2 = fract(52.9829189 * fract(dot(v.pos.xy, vec2f(0.06711056, 0.00583715))));
    let shown = pow(col, vec3f(1.0 / 2.2)) + (n2 - 0.5) / 255.0;
    return vec4f(pow(max(shown, vec3f(0.0)), vec3f(2.2)), 1.0);
}
