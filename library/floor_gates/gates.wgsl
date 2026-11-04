// Gates across the valley stage's lane, drawn by two geo.tex_instance nodes (body and carved
// front, gates.py writes the models). Render each 1 x 6: texel k is the k-th gate ahead of `near`.
// A gate stands on every `every`-th turn of the lane, across it, its front back along the lane.
// The camera flies along x = 0 and the lane swings to either side of it, so a gate stands half
// way between the two and opens wide enough for both: its pillars stand `clear` beyond the
// lane's free path and beyond the camera's, whichever is further out.
// A turn is where the lane stops swinging one way and starts back: where its sideways slope
// changes sign. Its number (the bars travelled at its middle over the bars between two turns)
// picks the turns and keeps each gate's own values as it comes nearer.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the shore (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: no gates.
// `what` place: where a gate's origin goes (rgb). aim: where its front points. size: its scale,
// 0 where none stands. rim: the light on its rims. glow: the light in its carving.
// @param what choice(place|aim|size|rim|glow) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the light pulses on its start
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): how strong the pulse is
// @param dawn float = 0 0..1 -- the stage's dawn: the gates catch more of the band's light towards 1
// @param swell float = 0 0..1 -- the stage's swell: the carving glows brighter
// @param warmth float = 0 0..1 -- 0 copper rims and ember carving, 1 jade
// @param presence float = 1 0..1 -- how many gates stand: towards 0 they sink into the water one after another, 0 none
// @param seed int = 0 0..1000 -- which turns hold a gate first, and each gate's own share of presence
// @param tall float = 3.8 1..8 -- a gate's height to the top of its crest, metres; the lintel's underside lies at 0.8 of it
// @param thick float = 0.6 0.1..3 -- a gate's depth, metres
// @param light float = 1 0..4 -- brightness of rims and carving
// @param pulse float = 0.25 0..1 -- how much the light brightens on each beat, times the bass
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 0.3 0..4 -- metres between a pillar and the free path of the lane or the camera
// @param swing float = 0.9 0..5 -- how far the stage's lane swings to either side (the stage's `swing`)
// @param every float = 2 1..8 -- a gate on every this many turns: 1 on each turn, 2 on every other
// @param near float = -6 -8..10 -- where the gates leave, behind the camera
// @param far float = 40 1..56 -- where they appear

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TEXELS: i32 = 256;
// gates.py: the foot below the water line, and the inner face of a pillar, as shares of the
// gate's height and width.
const BASE: f32 = 0.05;
const INNER: f32 = 0.398;
const TURNS: u32 = 12u;

fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

struct Gate {
    at: vec3f,
    aim: vec3f,
    size: vec3f,
}

fn gate(k: u32) -> Gate {
    var g: Gate;
    g.at = vec3f(0.0, 0.0, -50.0);
    g.aim = vec3f(0.0, -1.0, 0.0);
    g.size = vec3f(0.0);
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    if lane0.b <= 0.0 {
        return g;
    }
    let step = max(lane0.b, 0.01);
    // The turns in view, nearest first: where the slope changes sign.
    var ys: array<f32, TURNS>;
    var found = 0u;
    var before = lane0.g;
    for (var i = 1; i < TEXELS; i++) {
        let now = textureLoad(in0, vec2i(i, 0), 0).g;
        if (before < 0.0) != (now < 0.0) && before != now && found < TURNS {
            let f = before / (before - now);
            ys[found] = LANE_NEAR + (f32(i) - 0.5 + f) * (LANE_FAR - LANE_NEAR) / f32(TEXELS);
            found += 1u;
        }
        before = now;
    }
    if found == 0u {
        return g;
    }
    var bars = 4.0;
    if found > 1u {
        bars = max(round((ys[1] - ys[0]) / (4.0 * step)), 1.0);
    }
    let every = max(i32(round(params.every)), 1);
    let offset = i32(u32(max(params.seed, 0)) % u32(every));
    // The k-th turn beyond `near` whose number is a multiple of `every`.
    var seen = 0u;
    for (var j = 0u; j < found; j++) {
        let y = ys[j];
        if y < params.near {
            continue;
        }
        let at_bar = (params.beats + y / step) / 4.0 + params.drift;
        let turn = i32(floor(at_bar / bars));
        if ((turn + offset) % every + every) % every != 0 {
            continue;
        }
        if seen != k {
            seen += 1u;
            continue;
        }
        let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
        let lane = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
        let r = vec3f(pn_hash(vec3u(bitcast<u32>(turn), u32(max(params.seed, 0)), 83u))) / 4294967296.0;
        // Presence: each gate sinks at its own share of it; at a steady presence a gate stands
        // whole or not at all. One value per gate, so a later rise and fall of single gates can
        // join it here.
        let own = clamp((params.presence * 1.6 - r.x) * 8.0, 0.0, 1.0);
        let up = own * own * (3.0 - 2.0 * own);
        let half = params.lane + 0.5 * params.swing + params.clear;
        let wide = half / INNER;
        g.at = vec3f(0.5 * lane.r, y, -(1.0 + BASE) * params.tall * (1.0 - up));
        g.aim = normalize(vec3f(-lane.g, -1.0, 0.0));
        if y <= params.far && up >= 0.001 && on_floor(y, step) {
            g.size = vec3f(wide, params.thick, params.tall);
        }
        return g;
    }
    return g;
}

fn kick() -> f32 {
    let b = clamp(params.beat, 0.0, 1.0);
    return smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let g = gate(u32(v.pos.y));
    if params.what == what_place {
        return vec4f(g.at, 1.0);
    }
    if params.what == what_aim {
        return vec4f(g.aim, 1.0);
    }
    if params.what == what_size {
        return vec4f(g.size, 1.0);
    }
    let warm = clamp(params.warmth, 0.0, 1.0);
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let beat = params.pulse * kick() * (0.3 + clamp(params.bass, 0.0, 1.0));
    if params.what == what_rim {
        let tone = mix(vec3f(1.0, 0.5, 0.2), vec3f(0.45, 1.0, 0.75), warm);
        return vec4f(tone * params.light * (0.6 + 0.5 * dawn + 0.5 * swell) * (1.0 + beat), 1.0);
    }
    let ember = mix(vec3f(1.0, 0.42, 0.14), vec3f(0.5, 1.0, 0.78), warm);
    return vec4f(ember * params.light * (0.2 + 0.15 * dawn + 0.7 * swell) * (1.0 + 2.0 * beat), 1.0);
}
