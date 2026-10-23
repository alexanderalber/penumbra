// A box (a crate seen at a corner: front, side and top) as a tank of water: it fills
// from the bottom once when the effect starts, then stays, its surface rolling in waves along the
// front and round the corner onto the side, with caustics drifting below it. On the top a
// shallow pond, with rings running out from the foot of the object standing on it. The object
// gets the light the water throws up, warm, on its lower half. Nothing outside the three regions
// and the object is lit.
// in0: the regions (scan.regions' regions_camera.png through tex.image, 8-bit labels), in1: a
// labels picture (green: the object), in2: a water texture (tiles), in3: the distance from the
// object's silhouette (scan_labels' `sdf`: red in widths of the picture, negative inside).
// The front's and the side's corners (scan.regions' straight corners, camera px) give each a
// position on the box: u across from the left, v down from the top edge, so the water line is
// level on the box and not in the picture.
// @param clock float = -1 -1..100000 -- seconds since the effect started (bind a chan.clock); below 0: the show time
// @param front int = 0 0..255 -- region label of the box's front
// @param side int = 0 0..255 -- region label of the box's side
// @param lid int = 0 0..255 -- region label of the box's top; 0: no pond
// @param front_top vec4 = [0, 0, 1, 0] -- the front's top left and top right corners, camera px
// @param front_bottom vec4 = [0, 1, 1, 1] -- its bottom left and bottom right
// @param side_top vec4 = [0, 0, 1, 0] -- the side's top corners, the one at the front's corner first
// @param side_bottom vec4 = [0, 1, 1, 1]
// @param rows vec2 = [0, 1080] -- the object's top and bottom row, camera px
// @param fill float = 6 0.5..30 -- seconds the water takes to rise to its level
// @param pond float = 3 0..20 -- seconds the pond on the lid takes to come in once the tank is full
// @param level float = 0.12 0..0.5 -- where the water stands, as a share of the height down from the top edge
// @param wave float = 0.035 0..0.2 -- height of the waves, as a share of the height
// @param tile float = 22 2..100 -- one tile of the water texture, centimetres on the box
// @param caustic float = 9 1..60 -- size of the caustic cells, centimetres
// @param gain float = 1 0..3 -- brightness of the water
// @param deep color = [0.04, 0.3, 0.95, 1] -- the water at the bottom
// @param shallow color = [0.1, 0.75, 1, 1] -- the water under its surface
// @param foam color = [0.85, 0.97, 1, 1]
// @param shimmer float = 0.6 0..2 -- the water's light thrown on the object's lower half
// @param shimmer_color color = [1, 0.85, 0.5, 1] -- warm, as blue light turns an orange skin dark
// @param size vec3 = [60, 40, 32] -- the box's front width, side depth and height, centimetres

// A byte of an 8-bit picture read through tex.image (which decodes sRGB): its stored value.
fn byte(c: f32) -> i32 {
    let s = select(1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055, c * 12.92, c <= 0.0031308);
    return i32(round(s * 255.0));
}

fn cross2(a: vec2f, b: vec2f) -> f32 {
    return a.x * b.y - a.y * b.x;
}

// Where p lies in the quad a (top left), b (top right), c (bottom right), d (bottom left): u
// along the top edge, v down the left one (Inigo Quilez' inverse bilinear).
fn quad_uv(p: vec2f, a: vec2f, b: vec2f, c: vec2f, d: vec2f) -> vec2f {
    let e = b - a;
    let f = d - a;
    let g = a - b + c - d;
    let h = p - a;
    let k2 = cross2(g, f);
    let k1 = cross2(e, f) + cross2(h, g);
    let k0 = cross2(h, e);
    if (abs(k2) < 1e-3) {
        return vec2f((h.x * k1 + f.x * k0) / (e.x * k1 - e.y * k0), -k0 / k1);
    }
    let w = sqrt(max(k1 * k1 - 4.0 * k0 * k2, 0.0));
    var v = (-k1 - w) / (2.0 * k2);
    var u = (h.x - f.x * v) / (e.x + g.x * v);
    if (u < -0.2 || u > 1.2 || v < -0.2 || v > 1.2) {
        v = (-k1 + w) / (2.0 * k2);
        u = (h.x - f.x * v) / (e.x + g.x * v);
    }
    return vec2f(u, v);
}

// Caustics: the ridges of two noises drifting through each other, a net of bright lines.
fn caustics(q: vec2f, t: f32) -> f32 {
    let a = 1.0 - abs(pn_noise(vec3f(q, t * 0.35), 41u));
    let b = 1.0 - abs(pn_noise(vec3f(q * 1.6 + vec2f(3.1, 7.7), t * 0.45), 42u));
    return pow(a, 10.0) + 0.7 * pow(b, 12.0);
}

// The water line, as a share of the height down from the top, at s centimetres along the box.
fn surface(s: f32, t: f32, rest: f32) -> f32 {
    let w = 0.6 * sin(s * 0.11 + t * 1.3) + 0.3 * sin(s * 0.27 - t * 2.1)
        + 0.25 * pn_noise(vec3f(s * 0.05, t * 0.4, 0.0), 43u);
    return rest + params.wave * w;
}

// The tank's water at a point of the front or side: s across the box, v down, both on the box.
fn tank(s: f32, v: f32, t: f32, rest: f32, cam: vec2f) -> vec4f {
    let line = surface(s, t, rest);
    let below = v - line;
    let edge = 0.012;
    if (below < -edge) {
        return vec4f(0.0);
    }
    let cm = vec2f(s, v * params.size.z);
    let depth = clamp(below / max(1.0 - line, 0.05), 0.0, 1.0);
    let wob = vec2f(pn_noise(vec3f(cm * 0.08, t * 0.25), 44u), pn_noise(vec3f(cm * 0.08, t * 0.25 + 9.0), 45u));
    let tex = textureSampleLevel(in2, pn_repeat, cm / params.tile + wob * 0.06 + vec2f(t * 0.02, 0.0), 0.0).rgb;
    let body = mix(params.shallow.rgb, params.deep.rgb, sqrt(depth));
    // The texture's pale veins as a lighter shimmer in the body.
    let vein = smoothstep(0.35, 0.6, pn_luma(tex));
    var c = body * (0.75 + 0.5 * vein);
    let k = caustics(cm / params.caustic + wob * 0.3, t) * (1.0 - 0.6 * depth);
    c += vec3f(0.55, 0.9, 1.0) * k * 0.9;
    // Foam where the line is, a thin bright band rolling with it.
    let foam = 1.0 - smoothstep(0.0, edge, abs(below));
    let froth = foam * (0.7 + 0.3 * pn_noise(vec3f(s * 0.6, t * 1.5, 0.0), 46u));
    c = mix(c, params.foam.rgb, clamp(froth, 0.0, 1.0));
    let a = smoothstep(-edge, -edge * 0.3, below);
    return vec4f(c * params.gain * a, a);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let cam = v.pos.xy;
    let t = select(pn_time.seconds, params.clock, params.clock >= 0.0);
    let region = byte(textureLoad(in0, p, 0).r);
    let is_object = i32(round(textureLoad(in1, p, 0).g * 255.0)) > 0;
    // The tank fills: the water rises from the bottom edge to its level, easing in.
    let rise = smoothstep(0.0, params.fill, t);
    let rest = mix(1.05, params.level, 1.0 - (1.0 - rise) * (1.0 - rise));
    let full = smoothstep(params.fill * 0.8, params.fill * 0.8 + max(params.pond, 0.01), t);

    if (is_object) {
        // The water's light on the object, from its foot up to the middle, once the pond is in.
        let h = (cam.y - params.rows.x) / max(params.rows.y - params.rows.x, 1.0);
        let low = smoothstep(0.4, 0.95, h);
        let k = caustics(cam / 70.0, t * 1.2);
        let l = params.shimmer * full * low * (0.1 + 1.8 * k);
        return vec4f(params.shimmer_color.rgb * l, clamp(l, 0.0, 1.0));
    }
    if (region == params.front && params.front > 0) {
        let uv = quad_uv(cam, params.front_top.xy, params.front_top.zw, params.front_bottom.zw, params.front_bottom.xy);
        return tank(clamp(uv.x, 0.0, 1.0) * params.size.x, uv.y, t, rest, cam);
    }
    if (region == params.side && params.side > 0) {
        let uv = quad_uv(cam, params.side_top.xy, params.side_top.zw, params.side_bottom.zw, params.side_bottom.xy);
        return tank(params.size.x + clamp(uv.x, 0.0, 1.0) * params.size.y, uv.y, t, rest, cam);
    }
    if (region == params.lid && params.lid > 0 && full > 0.0) {
        // The pond: its own shallow water, seen at a slant (tiles half as tall), rings running out
        // from the object's foot and fading with the distance.
        let d = max(textureSampleLevel(in3, pn_clamp, v.uv, 0.0).r * f32(textureDimensions(in3).x), 0.0);
        let q = vec2f(cam.x, cam.y * 2.0) / 8.0;
        let ring = 0.5 + 0.5 * sin(d * 0.16 - t * 3.0 + 0.6 * pn_noise(vec3f(q * 0.05, t * 0.3), 47u));
        let rings = pow(ring, 6.0) * exp(-d / 220.0);
        let tex = textureSampleLevel(in2, pn_repeat, q / params.tile + vec2f(t * 0.015, 0.0), 0.0).rgb;
        var c = params.shallow.rgb * (0.55 + 0.45 * smoothstep(0.35, 0.6, pn_luma(tex)));
        c += vec3f(0.55, 0.9, 1.0) * (caustics(q / params.caustic, t) * 0.8 + rings * 0.9);
        let shore = (1.0 - smoothstep(4.0, 14.0, d)) * (0.6 + 0.4 * pn_noise(vec3f(cam * 0.08, t * 1.5), 48u));
        c = mix(c, params.foam.rgb, clamp(shore, 0.0, 1.0));
        // The pond floods in from its edges towards the object.
        let come = smoothstep(0.0, 0.15, full * 1.15 - (1.0 - clamp(d / 260.0, 0.0, 1.0)));
        return vec4f(c * params.gain * come, come);
    }
    return vec4f(0.0);
}
