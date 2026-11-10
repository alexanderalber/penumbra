// Turns the Gray-Scott state (r = 1 - U, g = V) into a picture: V as a lit relief, with a
// glowing rim on the slopes.
// Set this node's width and height to the output size; the state is scaled up smoothly.
//
// in0: the state. in1: the object mask, any size. in2: people, any size.
//
// @param colors palette = 263040 708599 59f2ff -- colours along V from the ground to the top of the pattern; the last one lights the rims
// @param top float = 0.55 0.05..1 -- the V at which the colours reach their top; lower makes thin patterns brighter
// @param relief float = 14 1..40 -- how steep the relief is: more gives sharper light and darker shade
// @param glow float = 1.0 0..3 -- the rim light on the slopes
// @param light float = 2.2 0..6.2832 -- direction the light comes from, in radians
// @param shine float = 0.55 0..2 -- highlights
// @param through float = 0 0..1 -- how much the mask shows as a lighter ground; 0 for projection onto the object
// @param marker float = 0.4 0..1 -- how much the people show as a light ring; 0 for projection
// @param mode choice(lit|flat|state) = lit -- lit: the relief; flat: only the colours; state: U and V raw (red 1 - U, green V)

fn sample_v(uv: vec2f) -> f32 {
    return textureSampleLevel(in0, pn_clamp, uv, 0.0).g;
}

// Colour i of the palette.
fn ground(i: i32) -> vec3f {
    return pn_palette(params.colors, i).rgb;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let st = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    if params.mode == mode_state {
        return vec4f(st.r, st.g, 0.0, 1.0);
    }
    let vv = st.g;
    let t = smoothstep(0.0, params.top, vv);
    // Without colours: grey-blue, with a cyan rim.
    var lo = vec3f(0.015, 0.026, 0.047);
    var hi = vec3f(0.164, 0.237, 0.325);
    var rim_c = vec3f(0.099, 0.893, 1.0);
    var base = mix(lo, hi, t);
    let count = params.colors.count;
    if count > 0 {
        // The ground runs over all colours but the last; the last lights the rims. With two
        // colours the ground blends from the first to the second, with one it is that one.
        let steps = select(max(count - 2, 0), 1, count == 2);
        let f = t * f32(steps);
        let i = min(i32(floor(f)), max(steps - 1, 0));
        base = mix(ground(i), ground(min(i + 1, max(steps, 0))), clamp(f - f32(i), 0.0, 1.0));
        lo = ground(0);
        hi = ground(steps);
        rim_c = pn_palette(params.colors, count - 1).rgb;
    }
    // The mask as a lighter ground, where no pattern covers it.
    let m = clamp(pn_luma(textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
    base = mix(base, base + (hi - lo) * 0.5 + vec3f(0.02), m * params.through * (1.0 - t));
    if params.mode == mode_flat {
        return vec4f(base, 1.0);
    }

    // Gradient in state texels, whatever the output size.
    let tx = 1.0 / vec2f(textureDimensions(in0));
    let g = vec2f(sample_v(v.uv + vec2f(tx.x, 0.0)) - sample_v(v.uv - vec2f(tx.x, 0.0)),
                  sample_v(v.uv - vec2f(0.0, tx.y)) - sample_v(v.uv + vec2f(0.0, tx.y))) * 0.5 * params.relief;
    let nrm = normalize(vec3f(-g, 1.0));
    let l = normalize(vec3f(cos(params.light), sin(params.light), 0.55));
    let h = normalize(l + vec3f(0.0, 0.0, 1.0));
    let diff = max(dot(nrm, l), 0.0);
    let spec = pow(max(dot(nrm, h), 0.0), 46.0) * params.shine;
    let rim = pow(1.0 - clamp(nrm.z, 0.0, 1.0), 2.6);
    var col = base * (0.34 + 0.90 * diff) + vec3f(spec) + rim_c * rim * params.glow;
    col = col / (1.0 + col * 0.28);

    // People as a thin light ring at the edge of their bright area.
    let pp = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
    let ring = smoothstep(0.15, 0.35, pp) * (1.0 - smoothstep(0.45, 0.7, pp));
    col = mix(col, vec3f(1.0), ring * params.marker * 0.7);
    return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), 1.0);
}
