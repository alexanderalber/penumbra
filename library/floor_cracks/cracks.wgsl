// Glowing cracks in the floor of the synthwave stage: the edges of a Voronoi pattern fixed to
// the floor, so a crack keeps its shape while it comes nearer. Light flows along them towards
// the camera, and the bass makes them glow. The picture covers the floor from `left` to `right`
// across and `near` to `far` ahead; floor_cracks lays it on a flat sheet just above the grid.
// Dark floor is transparent, so the grid shows through it; the glow's alpha follows its
// brightness. The lane stays dark.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// Out: colour premultiplied by alpha, as pictures between nodes are. Render it about 1280 x 1024.
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param presence float = 1 0..1 -- how much glows: 1 all, towards 0 the cracks go dark one cell after another, 0 none
// @param seed int = 1 0..1000 -- another number gives other cracks
// @param low float = 0 0..1 -- the bass level (energy_low of a chan.beat_grid): the cracks glow with it
// @param bass float = 0.7 0..1 -- share of the glow that comes and goes with the bass
// @param cell float = 1.6 0.3..8 -- metres across a cell between the cracks
// @param thickness float = 0.045 0.005..0.3 -- width of a crack's bright core, in metres
// @param glow float = 0.15 0..1 -- width of the light around a crack, in metres
// @param flow float = 0.6 -4..4 -- metres per beat the light flows along the cracks towards the camera, on top of the floor's motion
// @param pulse float = 3 0.5..20 -- metres between two waves of light along the floor
// @param color color = [1, 0.25, 0.08, 1] -- the cracks' colour
// @param hot color = [1, 0.85, 0.4, 1] -- the colour of a crack's core at full bass
// @param brightness float = 1.6 0..4 -- how bright the cracks are
// @param lane float = 1.6 0..5 -- half the width of the dark lane
// @param left float = -10 -20..0 -- the picture's left edge, metres across
// @param right float = 10 0..20 -- its right edge
// @param near float = -2 -8..10 -- its near edge, metres ahead
// @param far float = 14 1..40 -- its far edge
// @param fade float = 7 0..40 -- metres ahead where the cracks start to fade out
// @param gone float = 11 0..56 -- metres ahead where nothing is left of them
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

fn rand2(i: vec2i, seed: u32) -> vec2f {
    let h = pn_hash(vec3u(bitcast<vec2u>(i), seed));
    return vec2f(h.xy) / 4294967296.0;
}

// The distance to the nearest crack (a border between two cells), in cells, and the nearest
// cell's own number from 0 to 1. Two passes: the nearest point first, then the borders around
// it. `p` counts cells from row `shift` on, so cell (i, j) of `p` is cell (i, j + shift) of the
// floor.
fn borders(p: vec2f, shift: i32, seed: u32) -> vec2f {
    let n = vec2i(floor(p)) + vec2i(0, shift);
    let f = p - floor(p);
    var nearest = vec2i(0);
    var to = vec2f(0.0);
    var best = 8.0;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let g = vec2i(i, j);
            let r = vec2f(g) + rand2(n + g, seed) - f;
            let d = dot(r, r);
            if d < best {
                best = d;
                to = r;
                nearest = g;
            }
        }
    }
    var edge = 8.0;
    for (var j = -2; j <= 2; j++) {
        for (var i = -2; i <= 2; i++) {
            let g = nearest + vec2i(i, j);
            let r = vec2f(g) + rand2(n + g, seed) - f;
            let d = r - to;
            if dot(d, d) > 1e-5 {
                edge = min(edge, dot(0.5 * (to + r), normalize(d)));
            }
        }
    }
    let id = n + nearest;
    let order = f32(pn_hash(vec3u(bitcast<vec2u>(id), seed + 17u)).x) / 4294967296.0;
    return vec2f(edge, order);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let x = mix(params.left, params.right, v.uv.x);
    let y = mix(params.far, params.near, v.uv.y);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let seed = u32(max(params.seed, 0));
    let size = max(params.cell, 0.1);
    // The floor's own place: the floor moves `step` metres per beat towards the camera. The
    // whole metres of travel are split off so the pattern stays exact far into a long file.
    let moved = params.beats * step;
    let whole = floor(moved / size);
    let ground = vec2f(x / size, y / size + (moved / size - whole));
    let b = borders(ground, i32(whole), seed);
    let e = b.x * size;
    let core = 1.0 - smoothstep(0.2 * params.thickness, params.thickness, e);
    let halo = select(0.0, exp(-e / max(params.glow, 0.001)), params.glow > 0.0);
    // Waves of light running along the floor towards the camera.
    let along = (y + moved + params.flow * params.beats) / max(params.pulse, 0.1);
    let wave = 0.55 + 0.45 * sin(6.2831853 * fract(along));
    let bass = clamp(params.low, 0.0, 1.0);
    let beat = mix(1.0, 0.25 + 1.25 * bass, clamp(params.bass, 0.0, 1.0));
    // Presence: each cell goes dark at its own share of it.
    let own = clamp(params.presence * 2.0 - b.y, 0.0, 1.0);
    let shown = own * own * (3.0 - 2.0 * own);
    // The lane stays dark, a slanting lane a little wider.
    let off = abs(x - lane.r) - params.lane * (1.0 + 0.5 * abs(lane.g));
    let free = smoothstep(0.0, 0.5, off);
    let far = 1.0 - smoothstep(params.fade, params.gone, length(vec2f(x, y)));
    let light = (core + 0.45 * halo) * wave * beat * shown * free * far * clamp(lane.a, 0.0, 1.0)
        * params.brightness;
    let colour = mix(params.color.rgb, params.hot.rgb, clamp(core * bass * wave, 0.0, 1.0));
    let a = clamp(light, 0.0, 1.0);
    return vec4f(colour * a, a);
}
