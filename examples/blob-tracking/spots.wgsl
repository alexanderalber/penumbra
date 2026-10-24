// Glowing spots of light wandering over a dark ground: something to track without a camera.
// Each spot has a bright core, which the tracker counts, and a dimmer glow around it, which
// stays below the threshold.
// @param count int = 3 1..6 -- how many spots
// @param radius float = 0.07 0.01..0.2 -- of the core, in image heights
// @param pace float = 1 0..4 -- how fast they wander
// @param glow float = 0.5 0..1 -- brightness of the halo around each core
// @param look palette = neon -- the spots' colours, one each; empty: white
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let p = v.uv * vec2f(aspect, 1.0);
    let t = pn_time.seconds * params.pace;

    // A dark blue-violet ground, a little lighter in the middle.
    let edge = length(v.uv - vec2f(0.5)) * 1.4;
    var color = mix(vec3f(0.01, 0.009, 0.026), vec3f(0.002, 0.002, 0.006), clamp(edge, 0.0, 1.0));

    for (var i = 0; i < params.count; i++) {
        let f = f32(i);
        let centre = vec2f(
            aspect * (0.5 + 0.4 * sin(t * (0.31 + 0.07 * f) + f * 2.1)),
            0.5 + 0.38 * sin(t * (0.43 + 0.05 * f) + f * 1.3),
        );
        var hue = vec3f(1.0);
        if params.look.count > 0 {
            hue = pn_palette(params.look, i).rgb;
        }
        let r = length(p - centre) / params.radius;
        let core = 1.0 - smoothstep(0.6, 1.0, r);
        let halo = params.glow * exp(-1.6 * max(r - 0.8, 0.0)) * (1.0 - core);
        // The core: white in the middle, the spot's colour towards its rim.
        let rim = smoothstep(0.0, 0.9, r);
        color += mix(vec3f(1.0), hue, rim * 0.8) * core + hue * halo * 0.35;
    }
    return vec4f(min(color, vec3f(1.0)), 1.0);
}
