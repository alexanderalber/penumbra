// The picture: the trail through a palette (or coloured by where the agents head), with a glow,
// on a dark background, and the inputs drawn faintly over it.
// Set this node's width and height to the output size; the trail is scaled up smoothly.
//
// in0: the trail. in1: the trail blurred (the glow). in2: people, in3: food (any size).
// in4: fields.wgsl (g: where agents may not go).
//
// @param colors palette = 03001c 2a0b4f 7a1f73 d1495b f79d65 fff3b0 -- dark to bright: no trail to the brightest veins
// @param heading bool = false -- colours by the direction the agents travel instead of the palette
// @param gain float = 0.04 0.005..1 -- brightness per unit of trail
// @param contrast float = 1 0.3..3 -- above 1 thins the faint trails out
// @param glow float = 0.6 0..3 -- strength of the soft halo around the veins
// @param background color = [0.004, 0.004, 0.008, 1]
// @param through float = 0 0..1 -- how much the walls show as a lighter ground (mask_mode wall or inside, people_mode wall)
// @param marker float = 0.3 0..1 -- how much the people show, as a faint ring
// @param food_shown float = 0 0..1 -- how much the food shows, as dots
// @param show_inputs bool = true -- draws walls, people and food as through, marker and food_shown say; off: none of them, for projection onto the object

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let on = select(0.0, 1.0, params.show_inputs != 0u);
    let through = on * params.through;
    let marker = on * params.marker;
    let food_shown = on * params.food_shown;
    let t = textureSampleLevel(in0, pn_clamp, v.uv, 0.0).rgb;
    let g = textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb;
    let s = t.r + t.g + t.b;
    let sg = g.r + g.g + g.b;
    let b = pow(1.0 - exp(-params.gain * s), params.contrast);
    let bg = 1.0 - exp(-params.gain * sg);
    var col: vec3f;
    var halo: vec3f;
    if params.heading != 0u {
        let hue = t / max(t.r + t.g + t.b, 1e-6);
        let hg = g / max(g.r + g.g + g.b, 1e-6);
        // Weights of a third each are white; push the colours apart.
        col = b * clamp(0.5 + 2.4 * (hue - 1.0 / 3.0), vec3f(0.0), vec3f(1.5));
        halo = bg * clamp(0.5 + 2.4 * (hg - 1.0 / 3.0), vec3f(0.0), vec3f(1.5));
    } else {
        let base = pn_colormap(params.colors, 0.0).rgb;
        col = pn_colormap(params.colors, b).rgb - base;
        halo = pn_colormap(params.colors, 0.6 * bg).rgb - base;
    }
    var c = params.background.rgb + col + params.glow * halo * 0.5;
    if through > 0.0 || marker > 0.0 || food_shown > 0.0 {
        let wall = textureSampleLevel(in4, pn_clamp, v.uv, 0.0).g;
        let person = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
        let food = clamp(pn_luma(textureSampleLevel(in3, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
        let ground = vec3f(0.8, 0.85, 0.9);
        c = c + through * 0.25 * wall * ground;
        // People as a faint ring along the soft edge of their shape; food as dots.
        let ring = 4.0 * person * (1.0 - person);
        c = c + (marker * (0.35 * ring + 0.06 * person) + 0.5 * food_shown * food) * ground;
    }
    return vec4f(c, 1.0);
}
