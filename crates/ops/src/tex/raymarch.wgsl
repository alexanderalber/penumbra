// tex.raymarch: a volume seen through an orthographic camera looking along -z. vol0 is the
// volume (density in red, temperature in green), pic0 the glow's palette when `palette` is 1. Rays run in the box's own frame: `axis_x`, `axis_y`, `axis_z` are
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

// The glow's colour at temperature `t`, 0 cold to 1 hot: the palette's middle row, or black over
// red, orange and yellow to white.
fn glow_color(t: f32) -> vec3f {
    let x = clamp(t, 0.0, 1.0);
    if (params.palette == 1) {
        return max(textureSampleLevel(pic0, pn_clamp, vec2f(x, 0.5), 0.0).rgb, vec3f(0.0));
    }
    let r = smoothstep(0.05, 0.45, x);
    let g = smoothstep(0.3, 0.8, x);
    let b = smoothstep(0.7, 1.0, x);
    return vec3f(r, g * 0.85 + b * 0.15, b);
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
            let cell = textureSampleLevel(vol0, pn_clamp, at, 0.0);
            let density = max(cell.r, 0.0);
            let absorbed = 1.0 - exp(-params.thickness * density * step);
            var color = params.color.rgb;
            if (params.glow > 0.0) {
                color += params.glow * glow_color(cell.g / params.hot);
            }
            light += transmittance * absorbed * color;
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

// Where the point `q`, in metres from the box's centre in the box's own frame, shows in the
// world's x and y.
fn on_screen(q: vec3f) -> vec2f {
    return params.center.xy + params.axis_x.xy * q.x + params.axis_y.xy * q.y + params.axis_z.xy * q.z;
}

// How much a line a pixel and a half wide covers a point `d` metres from its middle.
fn line_cover(d: f32) -> f32 {
    return clamp(1.25 - d / max(params.line, 1e-6), 0.0, 1.0);
}

// The distance from `p` to the line between `a` and `b`.
fn to_segment(p: vec2f, a: vec2f, b: vec2f) -> f32 {
    let along = b - a;
    let t = clamp(dot(p - a, along) / max(dot(along, along), 1e-12), 0.0, 1.0);
    return length(p - a - along * t);
}

// The distance from `p` to the nearest edge of the box of half size `half` around `center`.
fn to_box_edges(p: vec2f, center: vec3f, half: vec3f) -> f32 {
    var nearest = 1e9;
    for (var corner = 0; corner < 8; corner++) {
        let signs = vec3f(f32(corner & 1), f32((corner >> 1) & 1), f32((corner >> 2) & 1)) * 2.0 - 1.0;
        let a = on_screen(center + half * signs);
        if (signs.x < 0.0) {
            nearest = min(nearest, to_segment(p, a, on_screen(center + half * vec3f(1.0, signs.y, signs.z))));
        }
        if (signs.y < 0.0) {
            nearest = min(nearest, to_segment(p, a, on_screen(center + half * vec3f(signs.x, 1.0, signs.z))));
        }
        if (signs.z < 0.0) {
            nearest = min(nearest, to_segment(p, a, on_screen(center + half * vec3f(signs.x, signs.y, 1.0))));
        }
    }
    return nearest;
}

// How much a line of the cell grid covers the point on a far side of the box along this ray, in
// metres from the box's centre. Lines are thinned out where cells would be under 4 pixels.
fn grid_cover(at: vec3f, half: vec3f) -> f32 {
    let on = abs(at) / max(half, vec3f(1e-6));
    let face = select(select(2, 1, on.y >= on.z), 0, on.x >= on.y && on.x >= on.z);
    var cover = 0.0;
    for (var axis = 0; axis < 3; axis++) {
        if (axis == face) {
            continue;
        }
        let thin = ceil(log2(max(4.0 * params.line / max(params.cell[axis], 1e-6), 1.0)));
        let spacing = params.cell[axis] * pow(2.0, thin);
        let f = fract((at[axis] + half[axis]) / spacing);
        cover = max(cover, line_cover(min(f, 1.0 - f) * spacing));
    }
    return cover;
}

// `color` over `under` by `cover`, premultiplied.
fn drawn(under: vec4f, color: vec3f, cover: f32) -> vec4f {
    return mix(under, vec4f(color, 1.0), cover);
}

// The picture with the grid and the shapes on top. pic0 the march, `pn_data` the shapes, two
// vectors each: the centre in metres from the box's centre and the kind (1 for a box, plus 2 for
// an obstacle); half the box's size and the sphere's radius.
@compute @workgroup_size(8, 8, 1)
fn overlay(@builtin(global_invocation_id) id: vec3u) {
    if (any(id.xy >= pn.size.xy)) {
        return;
    }
    var out = textureLoad(pic0, id.xy, 0);
    let uv = (vec2f(id.xy) + 0.5) / vec2f(pn.size.xy);
    let p = params.pan + (uv - 0.5) * vec2f(params.view.x, -params.view.y);
    if (params.grid == 1) {
        let half = params.box * 0.5;
        let world = vec3f(p, params.far);
        let ray = world - params.center;
        let local = vec3f(dot(ray, params.axis_x), dot(ray, params.axis_y), dot(ray, params.axis_z));
        let direction = -vec3f(params.axis_x.z, params.axis_y.z, params.axis_z.z);
        let span = slab(local / params.box + 0.5, direction / params.box);
        if (span.x < span.y) {
            out = drawn(out, vec3f(1.0), 0.14 * grid_cover(local + direction * span.y, half));
        }
        out = drawn(out, vec3f(1.0), 0.8 * line_cover(to_box_edges(p, vec3f(0.0), half)));
    }
    for (var i = 0; i < params.shapes; i++) {
        let at = pn_data[i * 2];
        let shape = pn_data[i * 2 + 1];
        let kind = i32(at.w);
        let color = select(vec3f(1.0, 0.55, 0.05), vec3f(0.15, 0.9, 1.0), kind >= 2);
        var d = 0.0;
        if ((kind & 1) == 1) {
            d = to_box_edges(p, at.xyz, shape.xyz);
        } else {
            d = abs(length(p - on_screen(at.xyz)) - shape.w);
        }
        out = drawn(out, color, line_cover(d));
    }
    textureStore(pic_out, id.xy, out);
}
