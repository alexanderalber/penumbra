// vol.smoke: Stable Fluids in three dimensions. Velocity volumes hold metres per second in
// xyz; field volumes hold the smoke's density in red, its temperature in green, its fuel in
// blue and the fuel burnt per second in alpha. Curl volumes hold the curl of the velocity in
// xyz and its length in w. Pressure
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

// Emitters: `params.emitters` of them in `pn_data`, four vectors each. The first holds the
// centre in metres from the box's lower corner and the shape (1 a box, 0 a sphere); the second
// half the box's size and the sphere's radius; the third the velocity and the share of the way
// a cell goes towards the emitter in one step; the fourth smoke, temperature and fuel.

// How much of the cell lies in emitter `i`: 1 inside, 0 outside, a cell-wide edge. A sphere of
// its radius, or a box of its half size on either side of its centre.
fn emitter(i: i32, id: vec3u) -> f32 {
    let at = pn_data[i * 4];
    let shape = pn_data[i * 4 + 1];
    let h = max(params.h, 1e-6);
    let d = cell_position(id) - at.xyz;
    if (at.w >= 0.5) {
        let inside = clamp((shape.xyz - abs(d)) / h + 0.5, vec3f(0.0), vec3f(1.0));
        return inside.x * inside.y * inside.z;
    }
    return clamp((shape.w - length(d)) / h + 0.5, 0.0, 1.0);
}

// Damping and fade together: each component loses `1 - keep` of itself or `fade`, whichever is
// more, and never more than it has. So damping slows what is strong, fade ends what is weak.
fn damped(v: vec4f, keep: f32, fade: f32) -> vec4f {
    let loss = min(abs(v), max(abs(v) * (1.0 - keep), vec4f(fade)));
    return v - sign(v) * loss;
}

// At most `max_speed`: a few cells a step. Faster than that the steps no longer hold the smoke
// together; a spike would grow until it blows the whole box empty.
fn limited(v: vec3f) -> vec3f {
    let speed = length(v);
    if (speed > params.max_speed) {
        return v * (params.max_speed / speed);
    }
    return v;
}

// Whether all six neighbours of `c` lie inside the box.
fn inner(c: vec3i) -> bool {
    return all(c >= vec3i(1)) && all(c <= vec3i(pn.size) - 2);
}

fn texture_coords(id: vec3u) -> vec3f {
    return (vec3f(id) + 0.5) / vec3f(pn.size);
}

// The curl of the velocity and its length. vol0 velocity.
@compute @workgroup_size(4, 4, 4)
fn curl(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let c = vec3i(id);
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    let dx = (velocity_at(c + x) - velocity_at(c - x)) / (2.0 * params.h);
    let dy = (velocity_at(c + y) - velocity_at(c - y)) / (2.0 * params.h);
    let dz = (velocity_at(c + z) - velocity_at(c - z)) / (2.0 * params.h);
    let w = clean(vec4f(dy.z - dz.y, dz.x - dx.z, dx.y - dy.x, 0.0)).xyz;
    textureStore(vol_out, id, vec4f(w, length(w)));
}

fn curl_length(c: vec3i) -> f32 {
    return textureLoad(vol2, clamp(c, vec3i(0), vec3i(pn.size) - 1), 0).w;
}

// Vorticity confinement: a push that spins up the eddies the grid smears out, towards where
// the curl is stronger, across it. vol2 curl.
fn confinement(c: vec3i, strength: f32) -> vec3f {
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    let grad = vec3f(
        curl_length(c + x) - curl_length(c - x),
        curl_length(c + y) - curl_length(c - y),
        curl_length(c + z) - curl_length(c - z),
    );
    let len = length(grad);
    if (len < 1e-6) {
        return vec3f(0.0);
    }
    let w = textureLoad(vol2, c, 0).xyz;
    return cross(grad / len, w) * (strength * params.h);
}

// Forces: buoyancy by temperature, vorticity confinement, damping and fade, the emitters'
// velocities in order. vol0 velocity, vol1 fields, vol2 curl (when `vort` > 0).
@compute @workgroup_size(4, 4, 4)
fn forces(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    var v = clean(textureLoad(vol0, id, 0)).xyz;
    let f = clean(textureLoad(vol1, id, 0));
    v += params.lift * f.g * params.dt;
    // Not at the box's edge: there the curl's neighbours are missing, and an open edge with
    // smoke flowing out spun up there until the whole box blew empty.
    if (params.vort > 0.0 && inner(vec3i(id))) {
        // How much of the strength acts here, 0 to 1.
        let mask = clamp(params.vort_constant + params.vort_velocity * length(v) + params.vort_smoke * f.r
            + params.vort_temp * f.g + params.vort_fuel * f.b, 0.0, 1.0);
        v += confinement(vec3i(id), params.vort * mask) * params.dt;
    }
    v = damped(vec4f(v, 0.0), params.vel_keep, params.vel_fade).xyz;
    for (var i = 0; i < params.emitters; i++) {
        let blow = pn_data[i * 4 + 2];
        v = mix(v, blow.xyz, clamp(emitter(i, id) * blow.w, 0.0, 1.0));
    }
    textureStore(vol_out, id, clean(vec4f(limited(v), 0.0)));
}

// Where the content of cell `id` comes from when velocity `v` carries it: `direction` 1 forward
// in time, -1 back. In texture coordinates.
fn carried_from(id: vec3u, v: vec3f) -> vec3f {
    return texture_coords(id) - params.direction * v * params.dt / params.box;
}

// Whether content from `source` counts: at open edges (`inflow` 1) what comes in from outside the
// box is clear air.
fn counts(source: vec3f) -> bool {
    let half = 0.5 / vec3f(pn.size);
    return params.inflow == 0 || params.edges == 1 || (all(source >= -half) && all(source <= 1.0 + half));
}

// One semi-Lagrangian step. vol0 what is carried, vol1 the velocity carrying it.
@compute @workgroup_size(4, 4, 4)
fn carry(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let source = carried_from(id, textureLoad(vol1, id, 0).xyz);
    var out = vec4f(0.0);
    if (counts(source)) {
        out = clean(textureSampleLevel(vol0, pn_clamp, source, 0.0));
    }
    textureStore(vol_out, id, out);
}

// MacCormack: the forward step `ahead` corrected by half the error a step back from it shows
// against `start`, kept within the cells the forward step read, so it cannot overshoot. Keeps
// edges and eddies sharp that one step smears. `v` the velocity at the cell.
fn sharpened(start: texture_3d<f32>, ahead: texture_3d<f32>, back: texture_3d<f32>, id: vec3u, v: vec3f) -> vec4f {
    let a = clean(textureLoad(ahead, id, 0));
    let source = texture_coords(id) - v * params.dt / params.box;
    if (!counts(source)) {
        return a;
    }
    let n = vec3i(pn.size);
    let base = vec3i(floor(source * vec3f(pn.size) - 0.5));
    var low = vec4f(3.0e38);
    var high = vec4f(-3.0e38);
    for (var k = 0; k < 8; k++) {
        let corner = clamp(base + vec3i(k & 1, (k >> 1) & 1, (k >> 2) & 1), vec3i(0), n - 1);
        let c = clean(textureLoad(start, corner, 0));
        low = min(low, c);
        high = max(high, c);
    }
    let corrected = a + 0.5 * (clean(textureLoad(start, id, 0)) - clean(textureLoad(back, id, 0)));
    return clamp(corrected, low, high);
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

fn burning_at(c: vec3i) -> f32 {
    return max(clean(textureLoad(vol2, clamp(c, vec3i(0), vec3i(pn.size) - 1), 0)).a, 0.0);
}

// The fuel burning per second around cell `c`, averaged with its neighbours: a source that jumps
// from cell to cell would leave a pattern of stripes the pressure steps cannot smooth.
fn burning_near(c: vec3i) -> f32 {
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    let around = burning_at(c + x) + burning_at(c - x) + burning_at(c + y) + burning_at(c - y)
        + burning_at(c + z) + burning_at(c - z);
    return (2.0 * burning_at(c) + around) / 8.0;
}

// The divergence, less the expansion of what burns, and last frame's pressure (scaled by `warm`)
// as the first guess. vol0 velocity, vol1 last pressure (when `warm` > 0), vol2 fields.
@compute @workgroup_size(4, 4, 4)
fn divergence(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let c = vec3i(id);
    let x = vec3i(1, 0, 0);
    let y = vec3i(0, 1, 0);
    let z = vec3i(0, 0, 1);
    var d = (velocity_at(c + x).x - velocity_at(c - x).x + velocity_at(c + y).y - velocity_at(c - y).y
        + velocity_at(c + z).z - velocity_at(c - z).z) / (2.0 * params.h);
    if (params.expansion != 0.0) {
        d -= params.expansion * burning_near(c);
    }
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
    textureStore(vol_out, id, clean(vec4f(limited(v), 0.0)));
}

// Smoke, temperature and fuel carried by the new velocity; then what is hot enough burns: fuel
// turns into heat and smoke. Then cooled and faded, and each emitter in order on what the one
// before left. vol0 fields, vol1 velocity; with
// `sharp` 1, vol2 and vol3 the fields one step forward and back from that.
@compute @workgroup_size(4, 4, 4)
fn advect_fields(@builtin(global_invocation_id) id: vec3u) {
    if (any(id >= pn.size)) {
        return;
    }
    let v = textureLoad(vol1, id, 0).xyz;
    var f = vec4f(0.0);
    if (params.sharp == 1) {
        f = sharpened(vol0, vol2, vol3, id, v);
    } else {
        let source = carried_from(id, v);
        if (counts(source)) {
            f = clean(textureSampleLevel(vol0, pn_clamp, source, 0.0));
        }
    }
    var smoke = max(f.r, 0.0);
    var temperature = f.g;
    var fuel = max(f.b, 0.0);
    var burnt = 0.0;
    if (params.burn_per_temp > 0.0 && fuel > 0.0 && temperature >= params.ignition) {
        burnt = params.burn_per_temp * max(min(temperature, 1.0) - params.ignition, 0.0) * params.dt;
        if (params.fuel_per_burn > 0.0) {
            burnt = min(burnt, fuel / params.fuel_per_burn);
        }
        fuel = max(fuel - params.fuel_per_burn * burnt, 0.0);
        temperature += params.temp_per_burn * burnt;
        smoke += params.smoke_per_burn * burnt;
    }
    smoke = damped(vec4f(smoke), params.smoke_keep, params.smoke_fade).x;
    // Temperatures run from -1 to 1: the heat of burning saturates instead of running away.
    temperature = clamp(temperature * params.temp_keep, -1.0, 1.0);
    for (var i = 0; i < params.emitters; i++) {
        let w = clamp(emitter(i, id) * pn_data[i * 4 + 2].w, 0.0, 1.0);
        let made = pn_data[i * 4 + 3];
        smoke = mix(smoke, made.x, w);
        temperature = clamp(mix(temperature, made.y, w), -1.0, 1.0);
        fuel = mix(fuel, made.z, w);
    }
    let rate = select(0.0, burnt / params.dt, params.dt > 0.0);
    textureStore(vol_out, id, clean(vec4f(smoke, temperature, fuel, rate)));
}
