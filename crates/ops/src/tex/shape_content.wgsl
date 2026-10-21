// tex.shape_content: content per shape from tex.shapes' `id` output (in0): red the shape's id,
// green a random value per shape, blue where along its outline, alpha the signed distance to its
// edge in pixels. The output has the input's size, so each pixel reads its own texel, unfiltered
// (filtering would blend the ids of neighbouring shapes).

const TAU: f32 = 6.28318530718;

// 1 for the first `duty` of every period of x, 0 for the rest, with edges `softness` of a
// period wide (split between rising and falling).
fn band(x: f32, duty: f32, softness: f32) -> f32 {
    let f = fract(x);
    let d = clamp(duty, 0.0, 1.0);
    let e = clamp(softness, 0.0, 1.0) * 0.5 * min(d, 1.0 - d);
    if (e <= 0.0) {
        return select(0.0, 1.0, f < d);
    }
    return smoothstep(0.0, e, f) * (1.0 - smoothstep(d - e, d, f));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let s = textureLoad(in0, vec2i(v.pos.xy), 0);
    let id = s.r;
    let random = s.g;
    let along = s.b;
    let sd = s.a;
    if (id < 0.0) {
        return vec4f(0.0);
    }
    let phase = pn_time.seconds * params.speed + params.stagger * id + params.scatter * random;
    // Inside the shape, antialiased over a pixel.
    var cover = clamp(0.5 - sd, 0.0, 1.0);
    var level = 1.0;
    switch params.mode {
        case mode_pulse: {
            let wave = 0.5 - 0.5 * cos(TAU * phase);
            level = mix(1.0, wave, clamp(params.depth, 0.0, 1.0));
        }
        case mode_chase: {
            cover = clamp(params.line * 0.5 + 0.5 - abs(sd), 0.0, 1.0);
            level = band(along * max(params.segments, 0.0) - phase, params.duty, params.softness);
        }
        case mode_ripple: {
            level = band(-sd / max(params.spacing, 0.01) - phase, params.duty, params.softness);
        }
        default: {}
    }
    let c = mix(params.color_a, params.color_b, random * clamp(params.variety, 0.0, 1.0));
    let a = clamp(c.a, 0.0, 1.0) * level * cover;
    return vec4f(c.rgb * a, a);
}
