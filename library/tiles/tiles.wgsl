// Glazed tiles with grout and a wooden rim on one rectified face. in0: the face's
// uv_K_camera.exr (or uv_K_N.exr for a projector's own pixels) through tex.image_file (red and green
// its u and v, 0 to 1; blue 1 on the face, less where something else hides it), in1: a tile
// atlas, the decorated tile in its left half and a plain one in its right; in2: the objects in the
// same pixels as in0 (objects_camera.png or labels_N.png through tex.image_file, 8-bit labels), of
// which `object` is the one the tiles leave out. Blue 1 (the face, also where the scan measured
// nothing) and 0.5 (an object other than the one left out: on a glossy top a scan leaves bits
// that fit no face) get tiles, 0.25 (another face seen inside the outline) does not.
// @param object int = 0 0..255 -- the object left out (0: none)
// @param reveal float = 1 0..1 -- 0 no tiles, 1 all; in between they appear one by one
// @param aspect vec2 = [60, 40] -- the face's width and depth, as given to scan.scene (cm)
// @param tile vec2 = [7.5, 12.5] -- one tile's width and depth, cm
// @param grout float = 0.5 0..2 -- width of the joints, cm
// @param rim float = 3 0..10 -- width of the wooden rim along the edge, cm
// @param decor float = 0.12 0..1 -- share of decorated tiles
// @param grout_color color = [0.08, 0.05, 0.03, 1]
// @param wood color = [0.55, 0.32, 0.13, 1]
// @param gain float = 1.6 0..4 -- brightness of the tiles
// @param seed float = 3 0..100

fn hash(c: vec2f) -> f32 {
    let h = pn_hash(vec3u(vec2u(vec2i(c) + 1000), u32(params.seed)));
    return f32(h.x & 0xffffu) / 65535.0;
}

// An 8-bit label from a picture tex.image_file decoded as sRGB.
fn label(t: texture_2d<f32>, p: vec2i) -> i32 {
    let c = textureLoad(t, p, 0).r;
    let s = select(1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055, c * 12.92, c <= 0.0031308);
    return i32(round(s * 255.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let m = textureLoad(in0, vec2i(v.pos.xy), 0);
    let left_out = params.object > 0 && label(in2, vec2i(v.pos.xy)) == params.object;
    if (m.b < 0.4 || m.r < 0.0 || m.g < 0.0 || left_out) {
        return vec4f(0.0);
    }
    // Centimetres on the face.
    let cm = m.rg * params.aspect;
    let inner = params.aspect - 2.0 * params.rim;
    let q = cm - params.rim;
    // The rim: wood with a grain along it.
    if (any(q < vec2f(0.0)) || any(q > inner)) {
        if (params.reveal <= 0.0) {
            return vec4f(0.0);
        }
        let along = select(cm.y, cm.x, min(q.y, inner.y - q.y) < min(q.x, inner.x - q.x));
        let across = select(cm.x, cm.y, min(q.y, inner.y - q.y) < min(q.x, inner.x - q.x));
        let grain = 0.75 + 0.25 * sin(across * 9.0 + pn_noise(vec3f(along * 0.15, across * 2.0, 0.0), 41u) * 4.0);
        let a = clamp(params.reveal * 2.0, 0.0, 1.0);
        return vec4f(params.wood.rgb * grain * params.gain * a, a);
    }
    // The tiles fill the inside exactly: as many as fit, stretched a little.
    let count = max(round(inner / params.tile), vec2f(1.0));
    let size = inner / count;
    let cell = floor(q / size);
    let local = (q - cell * size) / size;
    let joint = params.grout * 0.5 / size;
    let inside = all(local > joint) && all(local < 1.0 - joint);
    let order = hash(cell);
    let shown = smoothstep(order, order + 0.08, params.reveal * 1.08);
    if (shown <= 0.0) {
        return vec4f(0.0);
    }
    if (!inside) {
        return vec4f(params.grout_color.rgb * shown, shown);
    }
    let t = (local - joint) / (1.0 - 2.0 * joint);
    let decorated = hash(cell + vec2f(37.0, 11.0)) < params.decor;
    // Plain tiles turned at random, so they do not repeat.
    var tt = t;
    if (!decorated && hash(cell + vec2f(5.0, 3.0)) < 0.5) {
        tt = 1.0 - tt;
    }
    let atlas = vec2f(select(0.5, 0.0, decorated) + tt.x * 0.5, tt.y);
    var c = textureSampleLevel(in1, pn_clamp, atlas, 0.0).rgb;
    c *= 0.85 + 0.3 * hash(cell + vec2f(9.0, 1.0));
    // A tile just appearing flashes once.
    let flash = 1.0 + 1.5 * (1.0 - smoothstep(0.0, 1.0, shown)) * step(0.01, shown);
    return vec4f(c * params.gain * flash * shown, shown);
}
