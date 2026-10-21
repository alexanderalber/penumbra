// tex.fluid: a 2D fluid after Stam's Stable Fluids, one stage per pass, `stage` set by the node.
// The velocity lives on a coarse grid of square cells (the simulation), the dye at the output's
// resolution.
//
//   0 force     in0 the velocity, in1 the force picture, in2 the splats, in3 the obstacles:
//               adds the force, the splats and vorticity confinement; blue becomes how much
//               the force picture moves here (the dye pass injects by it)
//   1 advect    in0 the velocity, in3 the obstacles: carried along by itself, faded
//   2 diffuse   in0 the velocity so far, in1 the velocity before diffusing, in3 the obstacles:
//               one Jacobi step of viscosity
//   3 divergence in0 the velocity, in3 the obstacles
//   4 jacobi    in0 the pressure so far (times `warm`), in1 the divergence, in3 the obstacles:
//               one Jacobi step of the pressure equation
//   9 jacobi2   the same, two steps at once
//   5 gradient  in0 the velocity, in1 the pressure, in3 the obstacles: the velocity made free
//               of divergence
//   6 dye       in0 the dye, in1 the velocity, in2 the dye picture, in3 the splats: carried
//               along, faded, injected
//   7 resample  in0 anything, bilinear onto this target (a new size, a restored state)
//   8 velocity  in0 the velocity, in image sizes per second for other nodes
//
// Units: the velocity is in image heights per second on both axes (so the cells are square in
// the arithmetic), the pressure in heights squared per second, `h` is one cell in heights and
// `aspect` the grid's width over its height. Every value read from a texture goes through
// `pn_fl_clean`, so NaN or infinity anywhere (an input, a restored state) never spreads.

// Fastest velocity kept, in heights per second: across the whole picture in a sixteenth of a
// second.
const PN_FL_MAX_SPEED: f32 = 16.0;
// Brightest dye kept (premultiplied, linear).
const PN_FL_MAX_DYE: f32 = 64.0;
// The speed (heights per second) at which motion injects about 63 % of the full amount.
const PN_FL_SPEED_REF: f32 = 0.2;
// Most splats; the node writes at most this many columns.
const PN_FL_MAX_SPLATS: i32 = 64;

// Finite values within `limit`, else 0. Comparisons with NaN are false, so NaN fails the test
// as infinity does.
fn pn_fl_clean(v: vec4f, limit: f32) -> vec4f {
    let ok = abs(v) <= vec4f(1e30);
    return select(vec4f(0.0), clamp(v, vec4f(-limit), vec4f(limit)), ok);
}

fn pn_fl_dims() -> vec2i {
    return vec2i(pn.resolution);
}

fn pn_fl_inside(p: vec2i) -> bool {
    return all(p >= vec2i(0)) && all(p < pn_fl_dims());
}

// Whether the cell at p is solid: the obstacle picture's brightness above one half there.
fn pn_fl_solid(p: vec2i) -> bool {
    if (params.obstacles == 0) {
        return false;
    }
    let uv = (vec2f(p) + 0.5) * pn.texel;
    return pn_luma(textureSampleLevel(in3, pn_clamp, uv, 0.0).rgb) > 0.5;
}

fn pn_fl_vel(p: vec2i) -> vec2f {
    let q = clamp(p, vec2i(0), pn_fl_dims() - 1);
    return pn_fl_clean(textureLoad(in0, q, 0), PN_FL_MAX_SPEED).xy;
}

// The velocity a neighbour contributes to the divergence: across a wall or into an obstacle,
// the centre's mirrored on the axis that crosses it (nothing flows through); beyond an open
// edge, the edge's own.
fn pn_fl_side(p: vec2i, centre: vec2f, axis: i32) -> vec2f {
    let wall = !pn_fl_inside(p) && params.edges == 0;
    if (wall || pn_fl_solid(p)) {
        if (axis == 0) {
            return vec2f(-centre.x, centre.y);
        }
        return vec2f(centre.x, -centre.y);
    }
    return pn_fl_vel(p);
}

// Curl of the velocity at p, per second.
fn pn_fl_curl(p: vec2i) -> f32 {
    let l = pn_fl_vel(p - vec2i(1, 0));
    let r = pn_fl_vel(p + vec2i(1, 0));
    let t = pn_fl_vel(p - vec2i(0, 1));
    let b = pn_fl_vel(p + vec2i(0, 1));
    return ((r.y - l.y) - (b.x - t.x)) / (2.0 * params.h);
}

// Splat i of t: row 0 position (0..1) and velocity (heights per second), row 1 colour and
// radius (heights).
struct PnFlSplat { weight: f32, velocity: vec2f, color: vec3f }

fn pn_fl_splat(t: texture_2d<f32>, i: i32, uv: vec2f) -> PnFlSplat {
    let a = pn_fl_clean(textureLoad(t, vec2i(i, 0), 0), PN_FL_MAX_SPEED);
    let b = pn_fl_clean(textureLoad(t, vec2i(i, 1), 0), PN_FL_MAX_DYE);
    let d = (uv - a.xy) * vec2f(params.aspect, 1.0);
    let r = max(b.w, 1e-4);
    var s: PnFlSplat;
    s.weight = exp(-dot(d, d) / (r * r));
    s.velocity = a.zw;
    s.color = max(b.rgb, vec3f(0.0));
    return s;
}

fn pn_fl_moving(speed: f32) -> f32 {
    return 1.0 - exp(-max(speed, 0.0) / PN_FL_SPEED_REF);
}

fn pn_fl_force(v: PnVertex) -> vec4f {
    let p = vec2i(v.pos.xy);
    if (pn_fl_solid(p)) {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    var vel = pn_fl_vel(p);
    var weight = 0.0;
    if (params.force_in != 0) {
        // Motion in image sizes per input frame; force_k turns it into heights per second and
        // scales it by `force`.
        let f = pn_fl_clean(textureSampleLevel(in1, pn_clamp, v.uv, 0.0), 1e4).xy;
        let u = vec2f(f.x * params.aspect, f.y) * params.rate;
        vel += u * params.force * params.dt;
        weight = pn_fl_moving(length(u));
    }
    for (var i = 0; i < min(params.splats, PN_FL_MAX_SPLATS); i++) {
        let s = pn_fl_splat(in2, i, v.uv);
        vel += s.velocity * s.weight * params.force * params.dt;
    }
    if (params.vorticity > 0.0) {
        // Vorticity confinement (Fedkiw, Stam and Jensen 2001): a push along the curl's
        // rotation, towards where it is stronger, so the swirls the grid smooths away come back.
        let c = pn_fl_curl(p);
        let grad = vec2f(
            abs(pn_fl_curl(p + vec2i(1, 0))) - abs(pn_fl_curl(p - vec2i(1, 0))),
            abs(pn_fl_curl(p + vec2i(0, 1))) - abs(pn_fl_curl(p - vec2i(0, 1))),
        );
        let n = grad / (length(grad) + 1e-5);
        vel += vec2f(n.y, -n.x) * c * params.vorticity * params.h * 32.0 * params.dt;
    }
    return vec4f(pn_fl_clean(vec4f(vel, 0.0, 0.0), PN_FL_MAX_SPEED).xy, weight, 1.0);
}

fn pn_fl_advect(v: PnVertex) -> vec4f {
    let p = vec2i(v.pos.xy);
    if (pn_fl_solid(p)) {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let here = pn_fl_clean(textureLoad(in0, p, 0), PN_FL_MAX_SPEED);
    let back = v.uv - params.dt * vec2f(here.x / params.aspect, here.y);
    let carried = pn_fl_clean(textureSampleLevel(in0, pn_clamp, back, 0.0), PN_FL_MAX_SPEED).xy;
    // Blue (where the force moves) stays where it was measured.
    return vec4f(carried * params.vel_fade, here.z, 1.0);
}

fn pn_fl_diffuse(v: PnVertex) -> vec4f {
    let p = vec2i(v.pos.xy);
    let before = pn_fl_clean(textureLoad(in1, p, 0), PN_FL_MAX_SPEED);
    if (pn_fl_solid(p)) {
        return vec4f(0.0, 0.0, before.z, 1.0);
    }
    let sum = pn_fl_vel(p - vec2i(1, 0)) + pn_fl_vel(p + vec2i(1, 0)) + pn_fl_vel(p - vec2i(0, 1))
        + pn_fl_vel(p + vec2i(0, 1));
    let a = max(params.viscous, 0.0);
    return vec4f((before.xy + a * sum) / (1.0 + 4.0 * a), before.z, 1.0);
}

fn pn_fl_divergence(v: PnVertex) -> vec4f {
    let p = vec2i(v.pos.xy);
    let c = pn_fl_vel(p);
    let l = pn_fl_side(p - vec2i(1, 0), c, 0);
    let r = pn_fl_side(p + vec2i(1, 0), c, 0);
    let t = pn_fl_side(p - vec2i(0, 1), c, 1);
    let b = pn_fl_side(p + vec2i(0, 1), c, 1);
    let div = ((r.x - l.x) + (b.y - t.y)) / (2.0 * params.h);
    return vec4f(div, 0.0, 0.0, 1.0);
}

// The pressure at neighbour q of p: behind a wall or in an obstacle the centre's (no push
// across), beyond an open edge none.
fn pn_fl_pressure(q: vec2i, centre: f32) -> f32 {
    if (!pn_fl_inside(q)) {
        return select(0.0, centre, params.edges == 0);
    }
    if (pn_fl_solid(q)) {
        return centre;
    }
    return pn_fl_clean(textureLoad(in0, q, 0), 1e6).x * params.warm;
}

// One Jacobi step at cell p, from the pressure in in0.
fn pn_fl_step(p: vec2i) -> f32 {
    let q = clamp(p, vec2i(0), pn_fl_dims() - 1);
    let c = pn_fl_clean(textureLoad(in0, q, 0), 1e6).x * params.warm;
    let div = pn_fl_clean(textureLoad(in1, q, 0), 1e6).x;
    let sum = pn_fl_pressure(q - vec2i(1, 0), c) + pn_fl_pressure(q + vec2i(1, 0), c)
        + pn_fl_pressure(q - vec2i(0, 1), c) + pn_fl_pressure(q + vec2i(0, 1), c);
    return (sum - params.h * params.h * div) * 0.25;
}

fn pn_fl_jacobi(v: PnVertex) -> vec4f {
    return vec4f(pn_fl_step(vec2i(v.pos.xy)), 0.0, 0.0, 1.0);
}

// The neighbour q's pressure after one step, for the second step at a cell whose own is
// `centre`: the same rules as pn_fl_pressure.
fn pn_fl_stepped(q: vec2i, centre: f32) -> f32 {
    if (!pn_fl_inside(q)) {
        return select(0.0, centre, params.edges == 0);
    }
    if (pn_fl_solid(q)) {
        return centre;
    }
    return pn_fl_step(q);
}

// Two Jacobi steps in one pass: every pass costs the CPU about as much as a whole grid of
// texture reads costs the GPU, so fewer, wider passes are cheaper.
fn pn_fl_jacobi2(v: PnVertex) -> vec4f {
    let p = vec2i(v.pos.xy);
    let c = pn_fl_step(p);
    let div = pn_fl_clean(textureLoad(in1, p, 0), 1e6).x;
    let sum = pn_fl_stepped(p - vec2i(1, 0), c) + pn_fl_stepped(p + vec2i(1, 0), c)
        + pn_fl_stepped(p - vec2i(0, 1), c) + pn_fl_stepped(p + vec2i(0, 1), c);
    return vec4f((sum - params.h * params.h * div) * 0.25, 0.0, 0.0, 1.0);
}

fn pn_fl_gradient(v: PnVertex) -> vec4f {
    let p = vec2i(v.pos.xy);
    let here = pn_fl_clean(textureLoad(in0, p, 0), PN_FL_MAX_SPEED);
    if (pn_fl_solid(p)) {
        return vec4f(0.0, 0.0, here.z, 1.0);
    }
    let c = pn_fl_clean(textureLoad(in1, p, 0), 1e6).x;
    let l = pn_fl_pressure_at(p - vec2i(1, 0), c);
    let r = pn_fl_pressure_at(p + vec2i(1, 0), c);
    let t = pn_fl_pressure_at(p - vec2i(0, 1), c);
    let b = pn_fl_pressure_at(p + vec2i(0, 1), c);
    let vel = here.xy - vec2f(r - l, b - t) / (2.0 * params.h);
    return vec4f(pn_fl_clean(vec4f(vel, 0.0, 0.0), PN_FL_MAX_SPEED).xy, here.z, 1.0);
}

// As pn_fl_pressure, from in1.
fn pn_fl_pressure_at(q: vec2i, centre: f32) -> f32 {
    if (!pn_fl_inside(q)) {
        return select(0.0, centre, params.edges == 0);
    }
    if (pn_fl_solid(q)) {
        return centre;
    }
    return pn_fl_clean(textureLoad(in1, q, 0), 1e6).x;
}

fn pn_fl_dye(v: PnVertex) -> vec4f {
    let vel = pn_fl_clean(textureSampleLevel(in1, pn_clamp, v.uv, 0.0), PN_FL_MAX_SPEED);
    let back = v.uv - params.dt * vec2f(vel.x / params.aspect, vel.y);
    var dye = pn_fl_clean(textureSampleLevel(in0, pn_clamp, back, 0.0), PN_FL_MAX_DYE) * params.dye_fade;
    // Pouring in paints over what is there: the dye moves towards the colour poured, at
    // `amount` per second, so it never gets brighter than what comes in.
    let moving = params.moving != 0;
    let rate = max(params.amount, 0.0) * params.dt;
    if (params.dye_in != 0) {
        let src = pn_fl_clean(textureSampleLevel(in2, pn_clamp, v.uv, 0.0), PN_FL_MAX_DYE);
        let a = clamp(src.a, 0.0, 1.0);
        let colour = vec4f(src.rgb / max(a, 1e-4), 1.0);
        dye = mix(dye, colour, 1.0 - exp(-rate * a * select(1.0, vel.z, moving)));
    } else if (moving) {
        let a = clamp(params.color.a, 0.0, 1.0);
        dye = mix(dye, vec4f(params.color.rgb, 1.0), 1.0 - exp(-rate * a * vel.z));
    }
    for (var i = 0; i < min(params.splats, PN_FL_MAX_SPLATS); i++) {
        let s = pn_fl_splat(in3, i, v.uv);
        let w = select(1.0, pn_fl_moving(length(s.velocity)), moving);
        dye = mix(dye, vec4f(s.color, 1.0), 1.0 - exp(-rate * s.weight * w));
    }
    let clean = pn_fl_clean(dye, PN_FL_MAX_DYE);
    return vec4f(max(clean.rgb, vec3f(0.0)), clamp(clean.a, 0.0, 1.0));
}

fn pn_fl_resample(v: PnVertex) -> vec4f {
    return pn_fl_clean(textureSampleLevel(in0, pn_clamp, v.uv, 0.0), 1e4);
}

fn pn_fl_velocity(v: PnVertex) -> vec4f {
    let vel = pn_fl_clean(textureLoad(in0, vec2i(v.pos.xy), 0), PN_FL_MAX_SPEED);
    return vec4f(vel.x / params.aspect, vel.y, 0.0, 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    switch params.stage {
        case 0: { return pn_fl_force(v); }
        case 1: { return pn_fl_advect(v); }
        case 2: { return pn_fl_diffuse(v); }
        case 3: { return pn_fl_divergence(v); }
        case 4: { return pn_fl_jacobi(v); }
        case 5: { return pn_fl_gradient(v); }
        case 6: { return pn_fl_dye(v); }
        case 7: { return pn_fl_resample(v); }
        case 9: { return pn_fl_jacobi2(v); }
        default: { return pn_fl_velocity(v); }
    }
}
