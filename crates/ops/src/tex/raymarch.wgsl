// tex.raymarch: a volume seen through an orthographic camera looking along -z. vol0 is the
// volume (density in red). Rays run in the box's own frame: `axis_x`, `axis_y`, `axis_z` are
// the world's axes there (the box's rotation undone), positions in metres from its centre.

// Where the ray of a pixel enters and leaves the box in texture coordinates, as distances along
// it; enter > leave when it misses.
fn slab(origin: vec3f, direction: vec3f) -> vec2f {
    let inv = 1.0 / select(direction, vec3f(1e-12), abs(direction) < vec3f(1e-12));
    let a = (vec3f(0.0) - origin) * inv;
    let b = (vec3f(1.0) - origin) * inv;
    let near = min(a, b);
    let far = max(a, b);
    return vec2f(max(max(near.x, near.y), max(near.z, 0.0)), min(min(far.x, far.y), far.z));
}

@compute @workgroup_size(8, 8, 1)
fn march(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let uv = (vec2f(id.xy) + 0.5) / vec2f(pn.size.xy);
    // The view plane, in metres in the world, in front of everything.
    let world = vec3f(params.pan + (uv - 0.5) * vec2f(params.view.x, -params.view.y), params.far);
    let ray = world - params.center;
    let local = vec3f(dot(ray, params.axis_x), dot(ray, params.axis_y), dot(ray, params.axis_z));
    let direction = -vec3f(params.axis_x.z, params.axis_y.z, params.axis_z.z);
    // To texture coordinates: one metre is 1 / box along each axis.
    let origin = local / params.box + 0.5;
    let step_dir = direction / params.box;
    let span = slab(origin, step_dir);
    var transmittance = 1.0;
    var light = vec3f(0.0);
    if (span.x < span.y) {
        let step = params.step;
        let count = min(i32(ceil((span.y - span.x) / step)), 4096);
        // A different start for each pixel, so steps show as fine noise, not rings.
        let jitter = f32(pn_hash(vec3u(id.xy, 7u)).x >> 8u) / 16777216.0;
        var t = span.x + jitter * step;
        for (var i = 0; i < count; i++) {
            if (t > span.y) {
                break;
            }
            let at = origin + step_dir * t;
            let density = max(textureSampleLevel(vol0, pn_clamp, at, 0.0).r, 0.0);
            let absorbed = 1.0 - exp(-params.thickness * density * step);
            light += transmittance * absorbed * params.color.rgb;
            transmittance *= 1.0 - absorbed;
            if (transmittance < 0.002) {
                transmittance = 0.0;
                break;
            }
            t += step;
        }
    }
    let alpha = (1.0 - transmittance) * params.color.a;
    textureStore(pic_out, id.xy, vec4f(light * params.color.a, alpha));
}

// The opacity of a march as opaque grey. pic0 the march.
@compute @workgroup_size(8, 8, 1)
fn opacity(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    let a = textureLoad(pic0, id.xy, 0).a;
    textureStore(pic_out, id.xy, vec4f(a, a, a, 1.0));
}
