// The picture: light sand on a dark plate, or the nodal lines drawn straight from the field.
// in0: `deposit` (r: grains per pixel, g: how much they move), in1: `glow` (deposit blurred),
// in2: `field` (r: amplitude, b: on the plate). Its size is the size of in0.
// @param view choice(grains|lines|split|both) = grains -- grains: the sand; lines: the nodal lines from the field, no sand needed; split: sand left, lines right; both: lines faintly under the sand
// @param colors palette = 05070c 1a2233 5b6b80 c9cfd6 fff6e0 -- the sand, from few grains to many
// @param outline float = 0.35 0..1 -- how visible the plate's surface and edge are; 0 for projection onto the object itself
// @param show_plate float = 0 0..1 -- draws the plate's motion under the sand, in plate_color
// @param gain float = 0.5 0.05..3 -- brightness per grain
// @param glow float = 0.35 0..2 -- soft halo around dense sand
// @param moving float = 0.6 0..1 -- moving sand takes the colour hot
// @param hot color = [1.0, 0.45, 0.12, 1] -- the colour of moving sand
// @param line_width float = 2.5 0.5..10 -- views lines, split and both: width of the lines in pixels of the picture
// @param line_color color = [0.95, 0.92, 0.85, 1] -- views lines, split and both: the colour of the lines
// @param plate_color color = [0.15, 0.3, 0.6, 1] -- the colour of the plate's motion (show_plate)
// @param background color = [0.0, 0.0, 0.0, 1] -- off the plate

fn field(uv: vec2f) -> vec4f {
    return textureSampleLevel(in2, pn_clamp, uv, 0.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let view = params.view;
    let plate_motion = params.show_plate;
    let outline = params.outline;

    let px = 1.0 / pn.resolution;
    let f = field(v.uv);
    let on = f.b;
    // The plate: a slightly lighter surface and a thin edge.
    let ex = field(v.uv + vec2f(px.x, 0.0)).b - field(v.uv - vec2f(px.x, 0.0)).b;
    let ey = field(v.uv + vec2f(0.0, px.y)).b - field(v.uv - vec2f(0.0, px.y)).b;
    let edge = clamp(length(vec2f(ex, ey)), 0.0, 1.0);
    var c = params.background.rgb;
    c += outline * (0.035 * on * vec3f(0.6, 0.7, 0.9) + 0.25 * edge * vec3f(0.6, 0.7, 0.9));
    c += plate_motion * 0.35 * on * abs(f.r) * params.plate_color.rgb;

    // Nodal lines: where the amplitude is near 0, measured in pixels from the line.
    let a = abs(f.r);
    let gx = abs(field(v.uv + vec2f(px.x, 0.0)).r) - abs(field(v.uv - vec2f(px.x, 0.0)).r);
    let gy = abs(field(v.uv + vec2f(0.0, px.y)).r) - abs(field(v.uv - vec2f(0.0, px.y)).r);
    let slope = max(0.5 * length(vec2f(gx, gy)), 1e-5);
    let d = a / slope;
    let w = params.line_width;
    let ln = on * (1.0 - smoothstep(0.5 * w, w, d)) + on * 0.25 * (1.0 - smoothstep(w, 4.0 * w, d));

    // Sand.
    let t = textureSampleLevel(in0, pn_clamp, v.uv, 0.0).rg;
    let gl = textureSampleLevel(in1, pn_clamp, v.uv, 0.0).r;
    let b = 1.0 - exp(-params.gain * t.r);
    let base = pn_colormap(params.colors, 0.0).rgb;
    var sand = pn_colormap(params.colors, b).rgb - base;
    let mv = clamp(t.g / max(t.r, 1e-3), 0.0, 1.0) * params.moving;
    sand = mix(sand, b * params.hot.rgb, mv);
    let halo = (pn_colormap(params.colors, 0.5 * (1.0 - exp(-params.gain * gl))).rgb - base) * params.glow;

    let lines_c = ln * params.line_color.rgb;
    let sand_c = sand + halo;
    var res = c;
    if view == view_grains {
        res = c + sand_c;
    } else if view == view_lines {
        res = c + lines_c;
    } else if view == view_split {
        res = c + select(sand_c, lines_c, v.uv.x > 0.5) + select(0.0, 0.6, abs(v.uv.x - 0.5) < 0.5 * px.x) * vec3f(0.3);
    } else {
        res = c + 0.25 * lines_c + sand_c;
    }
    return vec4f(res, 1.0);
}
