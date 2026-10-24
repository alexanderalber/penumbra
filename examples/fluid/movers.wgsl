// Coloured spots wandering over black, a little mottled so their motion can be measured
// inside them too: a stand-in for a camera watching people move.
// @param count int = 3 1..6 -- how many spots
// @param radius float = 0.09 0.01..0.2 -- in image heights
// @param pace float = 1 0..4 -- how fast they wander
// @param look palette = neon -- the spots' colours, one each; empty: orange to blue
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let p = v.uv * vec2f(aspect, 1.0);
    let t = pn_time.seconds * params.pace;
    var color = vec3f(0.0);
    for (var i = 0; i < params.count; i++) {
        let f = f32(i);
        let centre = vec2f(
            aspect * (0.5 + 0.4 * sin(t * (0.31 + 0.07 * f) + f * 2.1)),
            0.5 + 0.38 * sin(t * (0.43 + 0.05 * f) + f * 1.3),
        );
        let d = p - centre;
        let inside = 1.0 - smoothstep(params.radius * 0.7, params.radius, length(d));
        // The mottling moves with the spot.
        let mottle = 0.75 + 0.25 * pn_noise(vec3f(d * 40.0, f), 7u);
        var hue = vec3f(1.0, 0.35, 0.1) * (1.0 - f / 3.0) + vec3f(0.1, 0.45, 1.0) * (f / 3.0);
        if params.look.count > 0 {
            hue = pn_palette(params.look, i).rgb;
        }
        color += hue * inside * mottle;
    }
    return vec4f(min(color, vec3f(1.0)), 1.0);
}
