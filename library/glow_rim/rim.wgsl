// A faint warm rim on the face an object stands on, right round its foot, as from light shining
// out low through its skin; only in front of the foot, where projectors' shadows of the object do
// not fall, and flickering with glow.wgsl's flame (the same noises).
// in0: the regions (an 8-bit label picture through tex.image), in1: the distance from the
// object's silhouette (scan_labels' `sdf`), in2: a labels picture (green the object).
// @param clock float = -1 -1..100000 -- seconds since the effect started (bind a chan.clock); below 0: the show time
// @param lid int = 0 0..255 -- region label of the face the object stands on; 0: no rim
// @param box vec4 = [0, 0, 1920, 1080] -- the object's bounding box in the camera picture, as glow.wgsl's
// @param reach float = 35 2..200 -- how far out from the foot the rim reaches, pixels
// @param below float = 0.78 0..1 -- the rim only below this share of the box's height
// @param gain float = 0.5 0..2
// @param flicker float = 0.35 0..1 -- as glow.wgsl's
// @param ignite float = 2 0..10 -- as glow.wgsl's
// @param color color = [1, 0.35, 0.04, 1]

fn byte(c: f32) -> i32 {
    let s = select(1.055 * pow(max(c, 0.0), 1.0 / 2.4) - 0.055, c * 12.92, c <= 0.0031308);
    return i32(round(s * 255.0));
}

fn flame(t: f32, depth: f32) -> f32 {
    let n = 0.55 * pn_noise(vec3f(t * 3.1, 0.0, 0.0), 51u) + 0.3 * pn_noise(vec3f(t * 9.0, 1.0, 0.0), 52u)
        + 0.15 * pn_noise(vec3f(t * 21.0, 2.0, 0.0), 53u);
    let gutter = smoothstep(0.45, 0.75, pn_noise(vec3f(t * 0.45, 3.0, 0.0), 54u));
    return 1.0 + depth * (0.6 * n - 0.45 * gutter);
}

fn lit(t: f32, secs: f32) -> f32 {
    if (secs <= 0.0) {
        return 1.0;
    }
    let s = clamp(t / secs, 0.0, 1.0);
    let sputter = 0.5 + 0.5 * pn_noise(vec3f(t * 14.0, 4.0, 0.0), 55u);
    return mix(s * s * sputter, 1.0, s * s);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    if (params.lid <= 0 || byte(textureLoad(in0, p, 0).r) != params.lid
        || i32(round(textureLoad(in2, p, 0).g * 255.0)) > 0) {
        return vec4f(0.0);
    }
    let size = max(params.box.zw - params.box.xy, vec2f(1.0));
    let down = (v.pos.y - params.box.y) / size.y;
    let d = max(textureSampleLevel(in1, pn_clamp, v.uv, 0.0).r * f32(textureDimensions(in1).x), 0.0);
    let t = select(pn_time.seconds, params.clock, params.clock >= 0.0);
    // Strongest right at the foot below the middle, fading out sideways and away from it.
    let across = (v.pos.x - (params.box.x + 0.5 * size.x)) / (0.5 * size.x);
    let w = smoothstep(params.below, params.below + 0.12, down) * exp(-across * across * 1.2)
        * exp(-d / max(params.reach * 0.4, 1.0));
    let i = params.gain * w * flame(t, params.flicker) * lit(t, params.ignite);
    return vec4f(params.color.rgb * i, clamp(i, 0.0, 1.0));
}
