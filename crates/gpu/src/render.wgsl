// The render node's passes (render.rs): a colour pass (material, input texture, part colours,
// texture coordinates or normals; phong materials lit by the lights, optionally with the
// resolution overlay) in two halves, the opaque pixels writing depth and the see-through ones
// drawn back to front over them; a data pass (texture coordinates, part id + 1, distance along
// the view), the measuring pass for res.analyze (the texture coordinates' derivatives per
// pixel), and per light casting shadows the shadow pass (depth only, see-through pixels left
// out) and the tint pass (what see-through surfaces let through, and the nearest one's depth).
// For ambient occlusion a prepass writes the opaque pixels' positions and normals (ao.wgsl takes
// it from there); the colour pass dims ambient and environment light by the result.
//
// Wireframes draw every triangle's corners on their own (vs_wire), with barycentric
// coordinates: a pixel's distance to the nearest edge, in pixels, gives the line's coverage in
// every pass; the faces between them show nothing, a fill colour, or nothing while hiding the
// lines behind them (drawn last: fs_hide writes the faces' depth, then fs_over, or the data and
// measuring passes' own, the lines where it is the nearest).
// Occluders draw first, write depth and leave the colour as it is.

// Alpha from which a pixel is opaque (render::OPAQUE).
const OPAQUE: f32 = 0.998;

// gpu::render::Light.
struct Light {
    // xyz: where it is; w: 0 ambient, 1 directional, 2 point, 3 spot, 4 projector,
    // 5 environment.
    position: vec4f,
    // xyz: where it shines to (unit), an environment light's turn in radians and its last mip;
    // w: cosine of half the spot's cone.
    direction: vec4f,
    // rgb: linear colour times intensity; w: cosine where the spot starts to fade.
    color: vec4f,
    // x: 1 where it fades with the square of the distance; y: its shadow map's layer + 1, 0
    // without shadows; z: its picture's slot + 1, 0 for white; w: the shadow bias's factor.
    extra: vec4f,
    // World to the light's clip space: its shadow map's, a projector's picture's.
    clip: mat4x4f,
};

struct Frame {
    clip: mat4x4f,
    view_z: vec4f,
    // w 1: the eye's position; w 0: a parallel view, xyz the direction to the viewer.
    eye: vec4f,
    mode: u32,
    light_count: u32,
    shade_all: u32,
    overlay: u32,
    // gpu::projection::Overlay: render pixels per needed projector pixel, the factor for a
    // coarser node behind the texture, and the cap where the render itself is too coarse.
    overlay_scale: vec2f,
    overlay_factor: f32,
    overlay_cap: f32,
    // x: texels across a shadow map; y: across a tint map; z: 1 where the tint maps hold
    // anything; w: 1 where ambient occlusion was drawn.
    shadow: vec4f,
    lights: array<Light, 16>,
};

struct Draw {
    world: mat4x4f,
    n0: vec4f,
    n1: vec4f,
    n2: vec4f,
    color: vec4f,
    // rgb: the highlight's colour; w: its exponent.
    specular: vec4f,
    emit: vec4f,
    part: u32,
    // 0 constant, 1 phong, 2 pbr, 3 occluder.
    shading: u32,
    metallic: f32,
    roughness: f32,
    // x: a wireframe's line width in pixels, 0 for a whole surface; y: 1 where the edges inside
    // flat polygons are left out; z: its faces, 0 open, 1 filled, 2 hidden.
    wire: vec4f,
    // The filled faces' colour, straight alpha.
    fill: vec4f,
    // x: 1 where drawn once per instance (geo::Instances); y: which instance textures are
    // there, bit 0 aim, 1 scale, 2 colour.
    inst: vec4u,
};

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var samp: sampler;
@group(1) @binding(0) var<uniform> draw: Draw;
@group(2) @binding(0) var tex: texture_2d<f32>;
// Instances: texel i % width, i / width of each is instance i's (geo::Instances).
@group(2) @binding(1) var inst_position: texture_2d<f32>;
@group(2) @binding(2) var inst_aim: texture_2d<f32>;
@group(2) @binding(3) var inst_scale: texture_2d<f32>;
@group(2) @binding(4) var inst_color: texture_2d<f32>;
// The colour pass only.
@group(3) @binding(0) var shadow_maps: texture_depth_2d_array;
@group(3) @binding(1) var shadow_samp: sampler_comparison;
@group(3) @binding(2) var picture0: texture_2d<f32>;
@group(3) @binding(3) var picture1: texture_2d<f32>;
@group(3) @binding(4) var picture2: texture_2d<f32>;
@group(3) @binding(5) var picture3: texture_2d<f32>;
@group(3) @binding(6) var picture_samp: sampler;
@group(3) @binding(7) var tint_maps: texture_2d_array<f32>;
// The environment light's maps (environment.rs): prefiltered for roughness by mip, and the
// irradiance over pi.
@group(3) @binding(8) var env_specular: texture_2d<f32>;
@group(3) @binding(9) var env_irradiance: texture_2d<f32>;
@group(3) @binding(10) var env_samp: sampler;
// Ambient occlusion (ao.wgsl's blur): visibility in red, the distance along the view in green.
@group(3) @binding(11) var ao_map: texture_2d<f32>;

const PI: f32 = 3.14159265;

struct VOut {
    // Invariant: hiding faces and their lines must reach the same depth in two pipelines.
    @builtin(position) @invariant pos: vec4f,
    @location(0) world: vec3f,
    @location(1) normal: vec3f,
    @location(2) uv: vec2f,
    // Wireframes: barycentric coordinates, and in w the bits of the edges inside flat polygons
    // (bit k: the edge opposite corner k).
    @location(3) edge: vec4f,
    // An instance's colour, multiplying the surface's; white without instances.
    @location(4) tint: vec3f,
};

@vertex
fn vs(
    @location(0) p: vec3f,
    @location(1) n: vec3f,
    @location(2) uv: vec2f,
    @builtin(instance_index) i: u32,
) -> VOut {
    return place(p, n, uv, i);
}

// Instance i's texel of t; `fallback` past the texture's end.
fn instance_texel(t: texture_2d<f32>, i: u32, fallback: vec4f) -> vec4f {
    let size = textureDimensions(t);
    let at = vec2u(i % max(size.x, 1u), i / max(size.x, 1u));
    if at.y >= size.y {
        return fallback;
    }
    return textureLoad(t, at, 0);
}

// A turn taking +y to `aim` with +z kept up as far as it can be: columns right, forward, up.
fn aim_turn(aim: vec3f) -> mat3x3f {
    let len = length(aim);
    if !(len > 1e-12) || !(len < 1e30) {
        return mat3x3f(vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), vec3f(0.0, 0.0, 1.0));
    }
    let f = aim / len;
    var r = cross(f, vec3f(0.0, 0.0, 1.0));
    if dot(r, r) < 1e-12 {
        r = vec3f(1.0, 0.0, 0.0);
    }
    r = normalize(r);
    return mat3x3f(r, f, cross(r, f));
}

fn place(p: vec3f, n: vec3f, uv: vec2f, i: u32) -> VOut {
    var model = p;
    var normal = n;
    var tint = vec3f(1.0);
    if draw.inst.x == 1u {
        let has = draw.inst.y;
        var turn = mat3x3f(vec3f(1.0, 0.0, 0.0), vec3f(0.0, 1.0, 0.0), vec3f(0.0, 0.0, 1.0));
        var scale = vec3f(1.0);
        if (has & 1u) != 0u {
            turn = aim_turn(instance_texel(inst_aim, i, vec4f(0.0)).xyz);
        }
        if (has & 2u) != 0u {
            scale = instance_texel(inst_scale, i, vec4f(1.0)).xyz;
        }
        if (has & 4u) != 0u {
            tint = max(instance_texel(inst_color, i, vec4f(1.0)).rgb, vec3f(0.0));
        }
        model = turn * (p * scale) + instance_texel(inst_position, i, vec4f(0.0)).xyz;
        // A flat scale squashes the normal to zero; the fragment stages then face the viewer.
        let inverse = select(vec3f(0.0), 1.0 / scale, abs(scale) > vec3f(1e-12));
        normal = turn * (n * inverse);
    }
    let w = draw.world * vec4f(model, 1.0);
    var o: VOut;
    o.pos = frame.clip * w;
    o.world = w.xyz;
    o.normal = mat3x3f(draw.n0.xyz, draw.n1.xyz, draw.n2.xyz) * normal;
    o.uv = uv;
    o.edge = vec4f(1.0, 1.0, 1.0, 0.0);
    o.tint = tint;
    return o;
}

@vertex
fn vs_wire(
    @location(0) p: vec3f,
    @location(1) n: vec3f,
    @location(2) uv: vec2f,
    @location(3) edge: vec4f,
    @builtin(instance_index) i: u32,
) -> VOut {
    var o = place(p, n, uv, i);
    o.edge = edge;
    return o;
}

// The surface's texture at the fragment, times its instance's colour.
fn texel(v: VOut) -> vec4f {
    return textureSample(tex, samp, v.uv) * vec4f(v.tint, 1.0);
}

// How much of the pixel a wireframe's lines cover; 1 on a whole surface. Uses derivatives:
// call in uniform control flow.
fn lines(v: VOut) -> f32 {
    let b = v.edge.xyz;
    let gx = dpdx(b);
    let gy = dpdy(b);
    let across = sqrt(gx * gx + gy * gy);
    if draw.wire.x <= 0.0 {
        return 1.0;
    }
    // Pixels to each edge; the edges inside flat polygons far away where they are left out.
    var d = b / max(across, vec3f(1e-9));
    if draw.wire.y > 0.5 {
        let inside = u32(v.edge.w + 0.5);
        d = select(d, vec3f(1e9), (vec3u(inside) & vec3u(1u, 2u, 4u)) != vec3u(0u));
    }
    let near = min(d.x, min(d.y, d.z));
    return clamp(0.5 * draw.wire.x + 0.5 - near, 0.0, 1.0);
}

// A distinct colour per part id: hues a golden angle apart.
fn palette(id: u32) -> vec3f {
    let h = fract(f32(id) * 0.6180339887 + 0.08);
    let k = vec3f(5.0, 3.0, 1.0);
    let p = abs(fract(vec3f(h) + k / 6.0) * 6.0 - 3.0);
    let rgb = mix(vec3f(1.0), clamp(p - 1.0, vec3f(0.0), vec3f(1.0)), 0.7) * 0.85;
    // To linear light, as every texture.
    return pow(rgb, vec3f(2.2));
}

// Texels of the bound texture per needed projector pixel along its worse axis.
fn density(uv: vec2f) -> f32 {
    return pn_density(uv * vec2f(textureDimensions(tex)), frame.overlay_scale, frame.overlay_factor, frame.overlay_cap);
}

// A projector's picture at `uv` (0..1 from the top left); white without one.
fn picture(slot: u32, uv: vec2f) -> vec3f {
    switch slot {
        case 1u: { return textureSampleLevel(picture0, picture_samp, uv, 0.0).rgb; }
        case 2u: { return textureSampleLevel(picture1, picture_samp, uv, 0.0).rgb; }
        case 3u: { return textureSampleLevel(picture2, picture_samp, uv, 0.0).rgb; }
        case 4u: { return textureSampleLevel(picture3, picture_samp, uv, 0.0).rgb; }
        default: { return vec3f(1.0); }
    }
}

// How much of `light` reaches `p` (normal `n`, facing the light), per colour: 1 lit, 0 in its
// shadow, coloured behind see-through surfaces; filtered over 3x3 texels. Outside its shadow
// map: lit.
fn unshadowed(light: Light, p: vec3f, n: vec3f) -> vec3f {
    let layer = u32(light.extra.y + 0.5);
    if layer == 0u {
        return vec3f(1.0);
    }
    let size = max(frame.shadow.x, 1.0);
    let c0 = light.clip * vec4f(p, 1.0);
    if c0.w <= 1e-9 {
        return vec3f(1.0);
    }
    // A shadow texel's width at p, in metres (the clip matrix's y row scales metres to clip
    // units at w = 1); p moves along its normal by one and a half of them, against acne.
    let row = vec3f(light.clip[0].y, light.clip[1].y, light.clip[2].y);
    let texel = 2.0 * c0.w / (size * max(length(row), 1e-12));
    let c = light.clip * vec4f(p + n * (1.5 * texel * light.extra.w), 1.0);
    if c.w <= 1e-9 {
        return vec3f(1.0);
    }
    let ndc = c.xyz / c.w;
    if abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0 || ndc.z < 0.0 || ndc.z > 1.0 {
        return vec3f(1.0);
    }
    let uv = ndc.xy * vec2f(0.5, -0.5) + 0.5;
    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let at = uv + vec2f(f32(x), f32(y)) / size;
            lit += textureSampleCompareLevel(shadow_maps, shadow_samp, at, i32(layer - 1u), ndc.z);
        }
    }
    return vec3f(lit / 9.0) * let_through(layer - 1u, uv, c.z);
}

// What the see-through surfaces between a light and a point let through, per colour: the tint
// map's colour where the point (clip depth `z`, as fs_tint keeps it) lies behind the nearest
// of them, else white; over 3x3 texels.
fn let_through(layer: u32, uv: vec2f, z: f32) -> vec3f {
    if frame.shadow.z < 0.5 {
        return vec3f(1.0);
    }
    let size = i32(max(frame.shadow.y, 1.0));
    let at = vec2i(floor(uv * f32(size)));
    var sum = vec3f(0.0);
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let texel = clamp(at + vec2i(x, y), vec2i(0), vec2i(size - 1));
            let t = textureLoad(tint_maps, texel, i32(layer), 0);
            // Half floats keep 11 bits: a little slack.
            sum += select(vec3f(1.0), t.rgb, z > t.a + 2e-3 * abs(t.a) + 1e-4);
        }
    }
    return sum / 9.0;
}

// Diffuse and highlight from every light at `p` with normal `n` (facing the viewer), the
// highlight with exponent `shininess`.
struct Lit {
    diffuse: vec3f,
    highlight: vec3f,
};

// Light from one directional, point, spot or projector light reaching `p` (normal `n`): the
// direction to it and its colour there, black where it does not reach.
struct Incoming {
    l: vec3f,
    rgb: vec3f,
};

fn incoming(light: Light, kind: u32, p: vec3f, n: vec3f) -> Incoming {
    var out: Incoming;
    out.l = -light.direction.xyz;
    out.rgb = vec3f(0.0);
    var reach = 1.0;
    var tint = vec3f(1.0);
    if kind == 4u {
        // Only inside the projector's frustum, its picture's colour there.
        let c = light.clip * vec4f(p, 1.0);
        if c.w <= 1e-9 {
            return out;
        }
        let ndc = c.xy / c.w;
        if abs(ndc.x) > 1.0 || abs(ndc.y) > 1.0 {
            return out;
        }
        tint = picture(u32(light.extra.z + 0.5), ndc * vec2f(0.5, -0.5) + 0.5);
    }
    // A parallel projector (an orthographic camera, its clip matrix's w row 0 0 0 1) throws
    // along its axis from everywhere alike, as a directional light.
    let parallel = kind == 4u
        && abs(light.clip[0].w) + abs(light.clip[1].w) + abs(light.clip[2].w) < 1e-12;
    if kind >= 2u && !parallel {
        let d = light.position.xyz - p;
        let dist2 = max(dot(d, d), 1e-8);
        out.l = d * inverseSqrt(dist2);
        if light.extra.x > 0.5 {
            reach = 1.0 / max(dist2, 1e-4);
        }
        if kind == 3u {
            let c = dot(-out.l, light.direction.xyz);
            reach *= smoothstep(light.direction.w, max(light.color.w, light.direction.w + 1e-4), c);
        }
    }
    if dot(n, out.l) <= 0.0 {
        return out;
    }
    out.rgb = light.color.rgb * tint * (reach * unshadowed(light, p, n));
    return out;
}

// Where direction `d` lies in an environment light's picture (environment.wgsl's equirect),
// the light turned by its `direction.x` radians about z.
fn env_uv(light: Light, d: vec3f) -> vec2f {
    let c = cos(light.direction.x);
    let s = sin(light.direction.x);
    let r = vec3f(c * d.x + s * d.y, c * d.y - s * d.x, d.z);
    return vec2f(0.5 - atan2(r.y, r.x) / (2.0 * PI), 0.5 - asin(clamp(r.z, -1.0, 1.0)) / PI);
}

// What a white matte surface with normal `n` shows under the environment light.
fn env_diffuse(light: Light, n: vec3f) -> vec3f {
    return textureSampleLevel(env_irradiance, env_samp, env_uv(light, n), 0.0).rgb * light.color.rgb;
}

// What a surface of `roughness` reflects along `r` from the environment light.
fn env_reflection(light: Light, r: vec3f, roughness: f32) -> vec3f {
    let lod = clamp(roughness, 0.0, 1.0) * light.direction.y;
    return textureSampleLevel(env_specular, env_samp, env_uv(light, r), lod).rgb * light.color.rgb;
}

// From `p` towards the viewer (not normalized): to the eye, or along a parallel view
// (`frame.eye.w` 0, `frame.eye.xyz` the direction).
fn to_viewer(p: vec3f) -> vec3f {
    return select(frame.eye.xyz, frame.eye.xyz - p, frame.eye.w > 0.5);
}

// A phong exponent as a roughness, for reflecting an environment.
fn phong_roughness(shininess: f32) -> f32 {
    return sqrt(2.0 / (shininess + 2.0));
}

// `open`: what ambient occlusion leaves of ambient and environment light.
fn lighting(p: vec3f, n: vec3f, shininess: f32, open: f32) -> Lit {
    var out: Lit;
    out.diffuse = vec3f(0.0);
    out.highlight = vec3f(0.0);
    let v = normalize(to_viewer(p));
    for (var i = 0u; i < min(frame.light_count, 16u); i++) {
        let light = frame.lights[i];
        let kind = u32(light.position.w + 0.5);
        if kind == 0u {
            out.diffuse += light.color.rgb * open;
            continue;
        }
        if kind == 5u {
            out.diffuse += env_diffuse(light, n) * open;
            out.highlight += env_reflection(light, reflect(-v, n), phong_roughness(shininess)) * open;
            continue;
        }
        let inc = incoming(light, kind, p, n);
        out.diffuse += inc.rgb * max(dot(n, inc.l), 0.0);
        let h = normalize(inc.l + v);
        out.highlight += inc.rgb * pow(max(dot(n, h), 0.0), shininess);
    }
    return out;
}

// Karis's fit of the split-sum's scale and bias for an environment's reflection (2014).
fn env_brdf(f0: vec3f, roughness: f32, ndv: f32) -> vec3f {
    let r = roughness * vec4f(-1.0, -0.0275, -0.572, 0.022) + vec4f(1.0, 0.0425, 1.04, -0.04);
    let a = min(r.x * r.x, exp2(-9.28 * ndv)) * r.x + r.y;
    let ab = vec2f(-1.04, 1.04) * a + r.zw;
    return f0 * ab.x + ab.y;
}

// A pbr material at `p` (normal `n`, facing the viewer) with straight colour `albedo` and
// alpha `a`: premultiplied diffuse plus reflections, which stay at full strength on
// see-through surfaces (glass reflects). Light units as phong's: a light of colour 1 head on
// gives a matte white surface 1, so the highlight carries the pi the diffuse leaves out. `open`
// as lighting()'s.
fn pbr(p: vec3f, n: vec3f, albedo: vec3f, a: f32, open: f32) -> vec3f {
    let v = normalize(to_viewer(p));
    let rough = clamp(draw.roughness, 0.03, 1.0);
    let alpha2 = pow(rough, 4.0);
    let metal = clamp(draw.metallic, 0.0, 1.0);
    let f0 = mix(vec3f(0.04), albedo, metal);
    let ndv = max(dot(n, v), 1e-4);
    let k = (rough + 1.0) * (rough + 1.0) / 8.0;
    var diffuse = vec3f(0.0);
    var reflected = vec3f(0.0);
    for (var i = 0u; i < min(frame.light_count, 16u); i++) {
        let light = frame.lights[i];
        let kind = u32(light.position.w + 0.5);
        if kind == 0u {
            diffuse += light.color.rgb * (1.0 - metal) * open;
            continue;
        }
        if kind == 5u {
            diffuse += env_diffuse(light, n) * (1.0 - metal) * open;
            reflected += env_reflection(light, reflect(-v, n), rough) * env_brdf(f0, rough, ndv) * open;
            continue;
        }
        let inc = incoming(light, kind, p, n);
        let ndl = dot(n, inc.l);
        if ndl <= 0.0 {
            continue;
        }
        let h = normalize(inc.l + v);
        let ndh = max(dot(n, h), 0.0);
        let f = f0 + (1.0 - f0) * pow(1.0 - max(dot(v, h), 0.0), 5.0);
        let dd = ndh * ndh * (alpha2 - 1.0) + 1.0;
        let d = alpha2 / max(PI * dd * dd, 1e-8);
        let g = ndl / (ndl * (1.0 - k) + k) * ndv / (ndv * (1.0 - k) + k);
        diffuse += (1.0 - f) * (1.0 - metal) * inc.rgb * ndl;
        reflected += PI * d * g * f / (4.0 * ndl * ndv) * inc.rgb * ndl;
    }
    return albedo * a * diffuse + reflected;
}

// A wireframe's faces: 0 open, 1 filled, 2 hidden; 0 for a whole surface.
fn faces() -> u32 {
    return select(0u, u32(draw.wire.z + 0.5), draw.wire.x > 0.0);
}

// What the surface shows before any light, a wireframe's lines (`line`: their coverage) over its
// faces: premultiplied.
fn surface(t: vec4f, line: f32) -> vec4f {
    let c = cover(t) * line;
    if faces() != 1u {
        return c;
    }
    let f = vec4f(draw.fill.rgb * draw.fill.a, draw.fill.a);
    return c + f * (1.0 - c.a);
}

// What the surface shows before any light: premultiplied, as the colour pass starts from.
fn cover(t: vec4f) -> vec4f {
    switch frame.mode {
        // The material: its colour (straight alpha) times its texture (premultiplied).
        case 0u: { return vec4f(draw.color.rgb * draw.color.a, draw.color.a) * t; }
        case 1u: { return t; }
        default: { return vec4f(1.0); }
    }
}

// What ambient occlusion leaves at this fragment: the pixel's own, unless it shows another
// surface (the map has one sample per pixel, the colour pass four at edges); then of its
// neighbours the one nearest the fragment's distance along the view. 1 where none is near (a
// surface the prepass did not see) or without occlusion.
fn occlusion(v: VOut) -> f32 {
    if frame.shadow.w < 0.5 {
        return 1.0;
    }
    let depth = dot(frame.view_z, vec4f(v.world, 1.0));
    let last = vec2i(textureDimensions(ao_map)) - 1;
    let at = vec2i(floor(v.pos.xy));
    let near = 0.01 * abs(depth) + 1e-4;
    let own = textureLoad(ao_map, clamp(at, vec2i(0), last), 0);
    if own.g > 0.0 && abs(own.g - depth) < near {
        return own.r;
    }
    var best = 1.0;
    var off = 3.0 * near;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let a = textureLoad(ao_map, clamp(at + vec2i(x, y), vec2i(0), last), 0);
            let d = abs(a.g - depth);
            if a.g > 0.0 && d < off {
                off = d;
                best = a.r;
            }
        }
    }
    return best;
}

// The colour pass's opaque pixels, writing depth.
@fragment
fn fs_solid(v: VOut) -> @location(0) vec4f {
    // An occluder: depth only, the colour as it is (it draws first, so that is the background).
    if draw.shading == 3u {
        return vec4f(0.0);
    }
    let c = shade(v, occlusion(v));
    if c.a < OPAQUE {
        discard;
    }
    return c;
}

// The colour pass's see-through pixels, over what is behind them.
@fragment
fn fs_blend(v: VOut) -> @location(0) vec4f {
    // Ambient occlusion holds what lies behind a see-through surface, not the surface.
    let c = shade(v, 1.0);
    if c.a >= OPAQUE || c.a < 0.002 {
        discard;
    }
    return c;
}

// `open`: what ambient occlusion leaves of ambient and environment light.
fn shade(v: VOut, open: f32) -> vec4f {
    let t = texel(v);
    // Derivatives in uniform control flow, before any branch.
    let r = density(v.uv);
    let line = lines(v);
    var n = v.normal;
    if dot(n, n) < 1e-12 {
        n = to_viewer(v.world);
    }
    n = normalize(n);
    var base: vec4f;
    switch frame.mode {
        case 0u, 1u: {
            base = surface(t, line);
        }
        case 2u: {
            base = vec4f(palette(draw.part), 1.0);
        }
        case 3u: {
            base = vec4f(fract(v.uv), 0.0, 1.0);
        }
        default: {
            base = vec4f(n * 0.5 + 0.5, 1.0);
        }
    }
    // Phong and pbr materials, and every one in the pictures geo and mat nodes show of
    // themselves (pbr there as phong without a highlight: a metal shows nothing but
    // reflections, and those pictures are about shape). Both sides answer alike: the normal is
    // turned to the viewer.
    let phong = draw.shading == 1u && frame.mode <= 1u;
    let facing = select(-n, n, dot(n, to_viewer(v.world)) >= 0.0);
    if draw.shading == 2u && frame.mode <= 1u && frame.shade_all == 0u {
        let albedo = clamp(base.rgb / max(base.a, 1e-6), vec3f(0.0), vec3f(1e4));
        let rgb = pbr(v.world, facing, albedo, base.a, open) + draw.emit.rgb * base.a;
        base = vec4f(rgb, base.a);
    } else if phong || (frame.shade_all == 1u && frame.mode != 4u) {
        let lit = lighting(v.world, facing, draw.specular.w, open);
        var rgb = base.rgb * lit.diffuse;
        if phong {
            rgb += (lit.highlight * draw.specular.rgb + draw.emit.rgb) * base.a;
        }
        base = vec4f(rgb, base.a);
    }
    if frame.overlay == 1u {
        let straight = clamp(base.rgb / max(base.a, 1e-6), vec3f(0.0), vec3f(1.0));
        base = vec4f(pn_overlay_tint(r, straight) * base.a, base.a);
    }
    return base;
}

// Wireframes with hidden faces: their depth, no colour (the pipeline writes none).
@fragment
fn fs_hide(v: VOut) -> @location(0) vec4f {
    return vec4f(0.0);
}

// ... then their lines where that depth is the nearest; the faces between leave the colour.
@fragment
fn fs_over(v: VOut) -> @location(0) vec4f {
    return shade(v, 1.0);
}

// For ambient occlusion, the opaque pixels as fs_solid draws them: the position and the distance
// along the view (0: nothing), and the normal turned to the viewer.
struct Prepass {
    @location(0) position: vec4f,
    @location(1) normal: vec4f,
};

@fragment
fn fs_prepass(v: VOut) -> Prepass {
    if surface(texel(v), lines(v)).a < OPAQUE {
        discard;
    }
    var n = v.normal;
    if dot(n, n) < 1e-12 {
        n = to_viewer(v.world);
    }
    n = normalize(n);
    var out: Prepass;
    out.position = vec4f(v.world, max(dot(frame.view_z, vec4f(v.world, 1.0)), 1e-6));
    out.normal = vec4f(select(-n, n, dot(n, to_viewer(v.world)) >= 0.0), 1.0);
    return out;
}

// Depth only, from a light: only what is opaque (as the colour pass's alpha) casts a full
// shadow; the tint pass takes the rest.
@fragment
fn fs_shadow(v: VOut) {
    if surface(texel(v), lines(v)).a < OPAQUE {
        discard;
    }
}

// From a light, a see-through surface: what it lets through, multiplied with what lies before
// it along the ray (the blending), and its clip depth in alpha (blended to the nearest). With
// alpha a and straight colour c it passes (1 - a) of the light, tinted towards c, fully from
// alpha one half: (1 - a) * mix(1, c, min(2a, 1)). Opaque passes nothing, invisible passes
// all, and a cut-out's colourless clear texels tint nothing. The opaque parts of a see-through
// surface go in as black too, though the shadow map holds them: this map has its own, coarser
// texels, and without them a seam of untinted light would show where opaque meets see-through.
@fragment
fn fs_tint(v: VOut) -> @location(0) vec4f {
    let c = surface(texel(v), lines(v));
    if c.a < 0.002 {
        discard;
    }
    let depth = (frame.clip * vec4f(v.world, 1.0)).z;
    if c.a >= OPAQUE {
        return vec4f(0.0, 0.0, 0.0, depth);
    }
    let straight = clamp(c.rgb / c.a, vec3f(0.0), vec3f(1.0));
    let through = (1.0 - c.a) * mix(vec3f(1.0), straight, min(2.0 * c.a, 1.0));
    return vec4f(through, depth);
}

// The measuring and data passes: an occluder writes nothing (as the cleared target) but hides
// what is behind it; a wireframe only where its lines cover at least half the pixel, one with
// filled faces as a whole surface.
@fragment
fn fs_measure(v: VOut) -> @location(0) vec4f {
    let d = vec4f(dpdx(v.uv), dpdy(v.uv));
    let line = lines(v);
    if draw.shading == 3u {
        return vec4f(0.0);
    }
    if line < 0.5 && faces() != 1u {
        discard;
    }
    return d;
}

@fragment
fn fs_data(v: VOut) -> @location(0) vec4f {
    let line = lines(v);
    if draw.shading == 3u {
        return vec4f(0.0);
    }
    if line < 0.5 && faces() != 1u {
        discard;
    }
    return vec4f(v.uv, f32(draw.part + 1u), dot(frame.view_z, vec4f(v.world, 1.0)));
}
