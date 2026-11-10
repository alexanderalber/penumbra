// Draws the sand for projection: grains with their own shade of the palette on a dark ground,
// lit from above so piles read as volume. Its width and height are the output size; every
// simulation cell becomes a block of pixels.
//
// in0: the state (from step). in1: the walls, drawn faintly. in2: a blurred copy of the state
// (depth), for the light on the piles. in3: the sources, drawn faintly with marker.
//
// @param palette palette = 8a5a2b b97c3c dca25a f2cf8e fff0d0 -- grain colours; a grain's colour value picks its place along them
// @param colors choice(time|source) = time -- follows the component's colors; time: the palette; source: the grain's colour value is a hue
// @param gain float = 1.0 0..3 -- brightness of the sand
// @param variation float = 0.35 0..1 -- how much each grain's brightness differs from its neighbours'
// @param shade float = 1.0 0..3 -- light from above on the surfaces of the piles
// @param depth float = 0.45 0..1 -- how much darker the inside of a pile is than its surface
// @param grain float = 0.25 0..1 -- draws each grain as a soft dot instead of a square
// @param wall_fill float = 0.015 0..1 -- how bright the walls are filled
// @param outline float = 0.2 0..1 -- how bright the edges of the walls are
// @param buried float = 0.25 0..1 -- how visible grains inside a wall are
// @param marker float = 0.0 0..1 -- how visible the sources are
// @param background color = [0.004, 0.004, 0.006, 1] -- the ground
// @param mode choice(lit|flat|state) = lit -- lit: colours and light; flat: colours only; state: the raw state channels

fn hue_rgb(h: f32) -> vec3f {
    let k = fract(vec3f(h) + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0));
    let c = clamp(abs(k * 6.0 - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
    return pow(mix(vec3f(1.0), c, 0.8), vec3f(2.2));
}

fn wall_at(uv: vec2f) -> f32 {
    let w = textureSampleLevel(in1, pn_clamp, uv, 0.0);
    return step(0.5, max(w.r, max(w.g, w.b)));
}

fn dens(uv: vec2f) -> f32 {
    return textureSampleLevel(in2, pn_clamp, uv, 0.0).r;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let size = vec2f(textureDimensions(in0));
    let cellf = v.uv * size;
    let cell = clamp(vec2i(cellf), vec2i(0), vec2i(size) - 1);
    let s = textureLoad(in0, cell, 0);
    if params.mode == mode_state {
        return vec4f(s.r, s.g, s.b, 1.0);
    }
    var col = params.background.rgb;
    let wall = wall_at(v.uv);
    // Walls: a faint fill and a brighter edge.
    let tx = 1.0 / size;
    let edge = max(abs(wall_at(v.uv + vec2f(tx.x, 0.0)) - wall_at(v.uv - vec2f(tx.x, 0.0))),
                   abs(wall_at(v.uv + vec2f(0.0, tx.y)) - wall_at(v.uv - vec2f(0.0, tx.y))));
    col += vec3f(0.55, 0.6, 0.7) * (wall * params.wall_fill + edge * wall * params.outline);
    if s.r > 0.5 {
        var base = vec3f(0.0);
        if params.colors == colors_source {
            base = hue_rgb(s.g);
        } else {
            base = pn_colormap(params.palette, s.g).rgb;
        }
        base *= 1.0 + params.variation * (s.b * 2.0 - 1.0);
        var c = base;
        if params.mode == mode_lit {
            // Normal from the blurred sand: surfaces facing up catch the light.
            let gx = dens(v.uv + vec2f(tx.x, 0.0)) - dens(v.uv - vec2f(tx.x, 0.0));
            let gy = dens(v.uv + vec2f(0.0, tx.y)) - dens(v.uv - vec2f(0.0, tx.y));
            let n = normalize(vec3f(-gx * 4.0, -gy * 4.0, 1.0));
            let l = normalize(vec3f(-0.35, -0.8, 0.5));
            let diff = max(dot(n, l), 0.0) - l.z;
            let d = dens(v.uv);
            let inner = smoothstep(0.55, 0.98, d);
            c = base * max(1.0 + params.shade * diff * 1.4, 0.15) * (1.0 - params.depth * inner);
            // A soft dot per grain.
            let f = fract(cellf) - 0.5;
            let r = length(f) * 2.0;
            c *= 1.0 - params.grain * smoothstep(0.55, 1.2, r);
        }
        c *= params.gain;
        if wall > 0.5 {
            c *= params.buried;
        }
        col = select(c, col + c, wall > 0.5);
    }
    let src = textureSampleLevel(in3, pn_clamp, v.uv, 0.0).rgb;
    col += src * params.marker * 0.5;
    return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), 1.0);
}
