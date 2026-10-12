// Surroundings for light.environment: an equirectangular picture (twice as wide as high) of a
// dim hall around the room, with a tall warm window towards -y (where the viewer stands) and a
// cool lamp overhead. The picture's middle looks along +x, as Blender has it.
// @param window float = 5 0..20 -- brightness of the window
// @param lamp float = 2 0..10 -- brightness of the lamp overhead
// @param high float = 3 0..20 -- brightness of the high windows all round
// @param drift float = 0.02 0..0.5 -- how fast the window's colour drifts
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let pi = 3.14159265;
    // Azimuth (0 along +x, counter-clockwise seen from above) and elevation.
    let phi = (0.5 - v.uv.x) * 2.0 * pi;
    let el = (0.5 - v.uv.y) * pi;
    // Walls dim and warm, darker towards the floor; the floor darker still.
    var c = mix(vec3f(0.12, 0.11, 0.1), vec3f(0.4, 0.38, 0.36), smoothstep(-0.2, 0.9, el));
    if el < 0.0 {
        c = vec3f(0.06, 0.05, 0.045);
    }
    // The window: 50 degrees wide around -y, from the floor line to 45 degrees up.
    let from_front = abs(atan2(sin(phi + 0.5 * pi), cos(phi + 0.5 * pi)));
    let across = 1.0 - smoothstep(0.38, 0.46, from_front);
    let up = smoothstep(0.02, 0.08, el) * (1.0 - smoothstep(0.72, 0.8, el));
    let hue = 0.5 + 0.5 * cos(pn_time.seconds * params.drift * 6.2831853 + vec3f(0.0, 2.1, 4.2));
    c += across * up * params.window * mix(vec3f(1.0, 0.82, 0.6), hue, 0.3);
    // Window bars: two dark lines across, one down the middle.
    let bars = (1.0 - smoothstep(0.0, 0.015, abs(el - 0.4))) + (1.0 - smoothstep(0.0, 0.02, from_front));
    c *= 1.0 - 0.9 * clamp(bars * across * up, 0.0, 1.0);
    // High windows all round, between 25 and 35 degrees up, with a mullion every 30 degrees:
    // what metal reflects from any side.
    let band = smoothstep(0.42, 0.45, el) * (1.0 - smoothstep(0.58, 0.61, el));
    let glass = abs(fract(phi / (pi / 6.0)) - 0.5) < 0.42;
    c += params.high * band * select(0.1, 1.0, glass) * vec3f(0.9, 0.95, 1.0);
    // The lamp: a disc of 20 degrees around straight up.
    c += params.lamp * vec3f(0.75, 0.85, 1.0) * smoothstep(1.35, 1.4, el);
    return vec4f(c, 1.0);
}
