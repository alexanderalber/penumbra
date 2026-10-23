// An object as the light of the scene: warm, flickering light on the faces around it, falling
// off with the distance from its silhouette in the camera picture.
// in0: a labels picture (scan_labels' `labels`), in1: the distance from the object's silhouette
// (scan_labels' `sdf`: its red is the signed distance in widths of the picture, negative inside).
// @param radius float = 150 20..1000 -- distance at which the light has fallen to about a third, pixels
// @param intensity float = 1.3 0..4
// @param flicker float = 0.45 0..1
// @param color color = [1, 0.38, 0.06, 1]
// @param wobble float = 25 0..100 -- how far the light's edge breathes, pixels

// A label of /clean: its red is the face, its green the object, k / 255.
fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    if (label(in0, p, 0) == 0 || label(in0, p, 1) > 0) {
        return vec4f(0.0);
    }
    let d0 = textureSampleLevel(in1, pn_clamp, v.uv, 0.0).r * f32(textureDimensions(in1).x);
    let t = pn_time.seconds;
    let breathe = params.wobble * pn_noise(vec3f(t * 1.7, 0.0, 0.0), 31u);
    let d = max(d0 + breathe, 0.0);
    let falloff = pow(1.0 / (1.0 + (d / params.radius) * (d / params.radius)), 1.5);
    let f = 1.0 - params.flicker * (0.5 + 0.5 * (0.55 * pn_noise(vec3f(t * 4.0, 1.0, 0.0), 32u)
        + 0.3 * pn_noise(vec3f(t * 13.0, 2.0, 0.0), 33u) + 0.15 * pn_noise(vec3f(t * 29.0, 3.0, 0.0), 34u)));
    let warm = mix(params.color.rgb, vec3f(1.0, 0.75, 0.4), falloff * falloff * falloff * 0.5);
    let light = params.intensity * falloff * f;
    let a = clamp(light, 0.0, 1.0);
    return vec4f(warm * light, a);
}
