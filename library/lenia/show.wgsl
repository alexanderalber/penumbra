// Turns the Lenia state into a picture for projection: dark ground, glowing bodies.
// Channel r runs through the palette `colors`; channels g and b (rule three) add their tints.
// Set this node's width and height to the output size; the state is scaled up smoothly.
//
// in0: the state (from step). in1: a blurred copy of the state, for the halo.
// in2: the object mask, in3: people (any size), drawn faintly as through and marker say.
//
// @param colors palette = 050d1f 0d73a6 d9fff2 -- colours of the bodies (channel r), from faint to full
// @param tint_g color = [0.04, 0.75, 0.2, 1] -- colour of channel g (rule three)
// @param tint_b color = [0.12, 0.25, 1.0, 1] -- colour of channel b (rule three)
// @param gain float = 1.3 0..4 -- brightness of the bodies
// @param glow float = 1.4 0..4 -- strength of the halo around the bodies
// @param relief float = 0.6 0..3 -- light on the slopes, a soft 3D look
// @param through float = 0 0..1 -- how much the mask shows as a lighter ground
// @param marker float = 0.15 0..1 -- how much the people show, as a faint ring
// @param show_inputs bool = true -- draws the mask and the people as through and marker say; off: neither, for projection onto the object
// @param mode choice(lit|flat|state) = lit -- lit: colours, relief and halo; flat: colours only; state: the raw channels

fn lin(c: vec3f) -> vec3f {
    return pow(c, vec3f(2.2));
}

// A palette colour back in sRGB: the ramp blends in sRGB and makes the result linear with lin.
fn srgb(c: vec3f) -> vec3f {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3f(0.0)), vec3f(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3f(0.0031308));
}

// The palette at u (0..1), blended in sRGB between its two nearest colours.
fn along(u: f32) -> vec3f {
    let n = f32(max(params.colors.count, 1));
    if n < 1.5 {
        return srgb(pn_colormap(params.colors, 0.0).rgb);
    }
    let x = clamp(u, 0.0, 1.0) * (n - 1.0);
    let i = min(floor(x), n - 2.0);
    let a = srgb(pn_colormap(params.colors, i / (n - 1.0)).rgb);
    let b = srgb(pn_colormap(params.colors, (i + 1.0) / (n - 1.0)).rgb);
    return mix(a, b, x - i);
}

// The colour of channel r at level t: the first half of the palette over the lower half of t,
// the rest over the upper half, faded in from nothing below 0.15.
fn ramp(t: f32) -> vec3f {
    let u = 0.5 * smoothstep(0.0, 0.5, t) + 0.5 * smoothstep(0.45, 1.0, t);
    return lin(along(u) * smoothstep(0.0, 0.15, t));
}

fn colour(a: vec3f) -> vec3f {
    var c = ramp(clamp(a.x, 0.0, 1.0));
    c += params.tint_g.rgb * a.y * a.y * 1.6;
    c += params.tint_b.rgb * a.z * a.z * 1.6;
    return c * params.gain;
}

fn st(uv: vec2f) -> vec3f {
    return textureSampleLevel(in0, pn_clamp, uv, 0.0).rgb;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let a = st(v.uv);
    if params.mode == mode_state {
        return vec4f(a, 1.0);
    }
    let on = select(0.0, 1.0, params.show_inputs != 0u);
    let through = on * params.through;
    let marker = on * params.marker;
    let ground = vec3f(0.004, 0.005, 0.008);
    var col = colour(a);
    if params.mode == mode_lit {
        // Relief from the gradient of the summed channels, in state texels.
        let tx = 1.0 / vec2f(textureDimensions(in0));
        let one = vec3f(1.0);
        let gx = dot(st(v.uv + vec2f(tx.x, 0.0)) - st(v.uv - vec2f(tx.x, 0.0)), one);
        let gy = dot(st(v.uv - vec2f(0.0, tx.y)) - st(v.uv + vec2f(0.0, tx.y)), one);
        let nrm = normalize(vec3f(-gx * 3.0, -gy * 3.0, 1.0));
        let l = normalize(vec3f(-0.5, 0.6, 0.6));
        let lit = max(dot(nrm, l), 0.0) - l.z;
        let rim = 1.0 - nrm.z;
        let top = colour(min(a + vec3f(0.4), vec3f(1.0)));
        col = col * max(1.0 + params.relief * lit * 1.5, 0.0) + top * rim * params.relief;
        // Halo from the blurred state.
        let b = textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb;
        col += colour(min(b * 1.5, vec3f(1.0))) * params.glow * 0.6;
    }
    if through > 0.0 || marker > 0.0 {
        let mk = pn_luma(textureSampleLevel(in2, pn_clamp, v.uv, 0.0).rgb);
        col += lin(vec3f(0.10, 0.12, 0.16)) * mk * through * 3.0;
        // People as a faint ring along the soft edge of their shape.
        let person = clamp(pn_luma(textureSampleLevel(in3, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
        let ring = 4.0 * person * (1.0 - person);
        col += vec3f(0.5, 0.5, 0.6) * (0.5 * ring + 0.15 * person) * marker;
    }
    col = ground + col / (1.0 + col * 0.3);
    return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), 1.0);
}
