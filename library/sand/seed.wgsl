// The start of the sand, at the simulation size (keep width and height even): nothing, a band
// of grains in coloured layers like sand art, or grains scattered over the whole picture.
// Grains are only put where in0 is not wall.
//
// in0: the wall picture (walls).
//
// @param start choice(empty|band|noise) = empty -- empty: no sand; band: rows full of sand from top to bottom; noise: grains scattered over the whole picture
// @param top float = 0.1 0..1 -- where the band starts, in picture heights from the top
// @param bottom float = 0.4 0..1 -- where the band ends
// @param layers int = 5 1..20 -- colour layers of the band, from top to bottom
// @param density float = 1.0 0..1 -- share of cells that get a grain
// @param seed int = 1 0..1000 -- another random start

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let w = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let wall = max(w.r, max(w.g, w.b)) > 0.5;
    let h = pn_hash(vec3u(u32(p.x), u32(p.y), u32(params.seed) * 7919u + 17u));
    let r0 = f32(h.x >> 8u) / 16777216.0;
    let r1 = f32(h.y >> 8u) / 16777216.0;
    let r2 = f32(h.z >> 8u) / 16777216.0;
    let layers = max(params.layers, 1);
    var on = false;
    var t = r1;
    if params.start == start_band {
        on = v.uv.y >= params.top && v.uv.y < params.bottom;
        // Layers with a wavy boundary.
        let f = (v.uv.y - params.top) / max(params.bottom - params.top, 1e-3);
        let wave = 0.04 * sin(v.uv.x * 9.0 + f32(params.seed)) + 0.02 * sin(v.uv.x * 23.0);
        let layer = floor(clamp(f + wave, 0.0, 0.999) * f32(layers));
        t = (layer + 0.5) / f32(layers) + (r1 - 0.5) * 0.08;
    } else if params.start == start_noise {
        on = true;
    }
    if on && !wall && r0 < params.density {
        return vec4f(1.0, clamp(t, 0.0, 1.0), r2, 1.0);
    }
    return vec4f(0.0, 0.0, 0.0, 1.0);
}
