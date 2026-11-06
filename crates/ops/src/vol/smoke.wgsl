// vol.smoke: Stable Fluids in three dimensions. Velocity volumes hold metres per second in
// xyz; field volumes hold the smoke's density in red, its temperature in green. Pressure
// volumes hold the pressure in red and the divergence it cancels in green. Cells are cubes of
// `params.h` metres; positions below are in metres from the box's lower corner.

// Clean what feeds back from frame to frame: a value that is not finite or absurdly large is 0.
fn clean(v: vec4f) -> vec4f {
    let bad = (v != v) | (abs(v) > vec4f(30000.0));
    return select(v, vec4f(0.0), bad);
}

fn cell_position(id: vec3u) -> vec3f {
    return (vec3f(id) + 0.5) * params.h;
}

// How much of the cell lies in the emitter's sphere: 1 inside, 0 outside, a cell-wide edge.
fn emitter(id: vec3u) -> f32 {
    let d = length(cell_position(id) - params.emit_at);
    return clamp((params.emit_radius - d) / max(params.h, 1e-6) + 0.5, 0.0, 1.0);
}

fn texture_coords(id: vec3u) -> vec3f {
    return (vec3f(id) + 0.5) / vec3f(pn.size);
}

// Forces: buoyancy by temperature, damping and fade, the emitter's velocity. vol0 velocity,
// vol1 fields.
@compute @workgroup_size(4, 4, 4)
fn forces(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    var v = clean(textureLoad(vol0, id, 0)).xyz;
    let f = clean(textureLoad(vol1, id, 0));
    v += params.lift * f.g * params.dt;
    v *= params.vel_keep;
    let speed = length(v);
    if (speed > 0.0) {
        v *= max(speed - params.vel_fade, 0.0) / speed;
    }
    v = mix(v, params.emit_velocity, emitter(id) * params.emit_rate);
    textureStore(vol_out, id, vec4f(v, 0.0));
}

// The velocity carried along by itself (semi-Lagrangian). vol0 velocity.
@compute @workgroup_size(4, 4, 4)
fn advect_velocity(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let v = textureLoad(vol0, id, 0).xyz;
    let source = texture_coords(id) - v * params.dt / params.box;
    let carried = textureSampleLevel(vol0, pn_clamp, source, 0.0).xyz;
    textureStore(vol_out, id, vec4f(carried, 0.0));
}

// Velocity at cell `c`, also outside the box: walls stand still, open edges continue the edge.
fn velocity_at(c: vec3i) -> vec3f {
    let n = vec3i(pn.size);
    if (any(c < vec3i(0)) || any(c >= n)) {
        if (params.edges == 1) {
            return vec3f(0.0);
        }
        return textureLoad(vol0, clamp(c, vec3i(0), n - 1), 0).xyz;
    }
    return textureLoad(vol0, c, 0).xyz;
}

// The divergence, and last frame's pressure (scaled by `warm`) as the first guess. vol0
// velocity, vol1 last pressure (when `warm` > 0).
@compute @workgroup_size(4, 4, 4)
fn divergence(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let c = vec3i(id);
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    let d = (velocity_at(c + x).x - velocity_at(c - x).x + velocity_at(c + y).y - velocity_at(c - y).y
        + velocity_at(c + z).z - velocity_at(c - z).z) / (2.0 * params.h);
    var p = 0.0;
    if (params.warm > 0.0) {
        p = clean(textureLoad(vol1, id, 0)).r * params.warm;
    }
    textureStore(vol_out, id, vec4f(p, d, 0.0, 0.0));
}

// Pressure at cell `c`; outside the box 0 at open edges (the fluid leaves freely), the cell's
// own at walls (no push through them).
fn pressure_at(c: vec3i, own: f32) -> f32 {
    let n = vec3i(pn.size);
    if (any(c < vec3i(0)) || any(c >= n)) {
        return select(0.0, own, params.edges == 1);
    }
    return textureLoad(vol0, c, 0).r;
}

// One Jacobi step on the pressure. vol0 pressure.
@compute @workgroup_size(4, 4, 4)
fn jacobi(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let c = vec3i(id);
    let here = textureLoad(vol0, id, 0);
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    let sum = pressure_at(c + x, here.r) + pressure_at(c - x, here.r) + pressure_at(c + y, here.r)
        + pressure_at(c - y, here.r) + pressure_at(c + z, here.r) + pressure_at(c - z, here.r);
    let p = (sum - params.h * params.h * here.g) / 6.0;
    textureStore(vol_out, id, vec4f(p, here.g, 0.0, 0.0));
}

fn pressure_of(c: vec3i, own: f32) -> f32 {
    let n = vec3i(pn.size);
    if (any(c < vec3i(0)) || any(c >= n)) {
        return select(0.0, own, params.edges == 1);
    }
    return textureLoad(vol1, c, 0).r;
}

// The pressure's gradient taken off: what is left has no divergence. vol0 velocity, vol1
// pressure.
@compute @workgroup_size(4, 4, 4)
fn project(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let c = vec3i(id);
    let own = textureLoad(vol1, id, 0).r;
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    let gradient = vec3f(
        pressure_of(c + x, own) - pressure_of(c - x, own),
        pressure_of(c + y, own) - pressure_of(c - y, own),
        pressure_of(c + z, own) - pressure_of(c - z, own),
    ) / (2.0 * params.h);
    var v = textureLoad(vol0, id, 0).xyz - gradient;
    if (params.edges == 1) {
        // Nothing through a wall: the edge cells lose the part of their motion into it.
        let n = vec3i(pn.size);
        let low = c == vec3i(0);
        let high = c == n - 1;
        v = select(v, min(v, vec3f(0.0)), high);
        v = select(v, max(v, vec3f(0.0)), low);
    }
    textureStore(vol_out, id, vec4f(v, 0.0));
}

// Smoke and temperature carried by the new velocity, then cooled, faded and emitted. vol0
// fields, vol1 velocity.
@compute @workgroup_size(4, 4, 4)
fn advect_fields(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let v = textureLoad(vol1, id, 0).xyz;
    let source = texture_coords(id) - v * params.dt / params.box;
    var f = vec4f(0.0);
    // At open edges what comes in from outside is clear air.
    let half = 0.5 / vec3f(pn.size);
    if (params.edges == 1 || (all(source >= -half) && all(source <= 1.0 + half))) {
        f = clean(textureSampleLevel(vol0, pn_clamp, source, 0.0));
    }
    let smoke = max(f.r * params.smoke_keep - params.smoke_fade, 0.0);
    let temperature = f.g * params.temp_keep;
    let w = emitter(id) * params.emit_rate;
    let out = vec4f(mix(smoke, params.emit_smoke, w), mix(temperature, params.emit_temp, w), 0.0, 0.0);
    textureStore(vol_out, id, out);
}
