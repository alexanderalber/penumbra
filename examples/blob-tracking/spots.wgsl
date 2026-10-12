// Soft spots of light wandering over black: something to track without a camera.
// @param count int = 3 1..6 -- how many spots
// @param radius float = 0.07 0.01..0.2 -- in image heights
// @param pace float = 1 0..4 -- how fast they wander
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let p = v.uv * vec2f(aspect, 1.0);
    let t = pn_time.seconds * params.pace;
    var light = 0.0;
    for (var i = 0; i < params.count; i++) {
        let f = f32(i);
        let centre = vec2f(
            aspect * (0.5 + 0.4 * sin(t * (0.31 + 0.07 * f) + f * 2.1)),
            0.5 + 0.38 * sin(t * (0.43 + 0.05 * f) + f * 1.3),
        );
        light += 1.0 - smoothstep(params.radius * 0.6, params.radius, length(p - centre));
    }
    return vec4f(vec3f(min(light, 1.0)), 1.0);
}
