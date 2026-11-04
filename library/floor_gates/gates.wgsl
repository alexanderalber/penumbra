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
// memory: what each gate remembers from frame to frame. Render it 2 x 32 and feed it back into
// itself through a tex.feedback into its own in1; the other nodes read it in their in1. A gate
// that presence brings or takes rises or sinks over `ease` bars; within `hold` metres of the
// camera it keeps what it does until it has passed, so it never sinks or rises right in front of
// the camera. Texel (0, n modulo 32) holds the gate at turn n: red 1 if it stands by presence
// plus 4 times its lap, green how far it has risen, alpha its distance ahead; texel (1, 0) the
// clock (whole beats modulo 1024, the beat's fraction).
// @param what choice(place|aim|size|rim|glow|memory) = place -- what this node writes
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
// @param ease float = 2 0.25..8 -- bars a gate takes to rise or sink when presence brings or takes it
// @param hold float = 9 0..30 -- metres ahead within which a gate keeps what presence gave it until it has passed

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TEXELS: i32 = 256;
// gates.py: the foot below the water line, and the inner face of a pillar, as shares of the
// gate's height and width.
const BASE: f32 = 0.05;
const INNER: f32 = 0.398;
const TURNS: u32 = 12u;
// The memory: turns it holds.
const SLOTS: f32 = 32.0;

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

// The turns in view, nearest first, and the bars between two of them.
struct Turns {
    ys: array<f32, TURNS>,
    found: u32,
    bars: f32,
}

fn turns() -> Turns {
    var t: Turns;
    t.found = 0u;
    t.bars = 4.0;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let step = max(lane0.b, 0.01);
    // Where the slope changes sign. Filled in a local array: some backends cannot index into a
    // struct's array to write it.
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
    t.ys = ys;
    t.found = found;
    if found > 1u {
        t.bars = max(round((ys[1] - ys[0]) / (4.0 * step)), 1.0);
    }
    return t;
}

fn turn_number(y: f32, bars: f32) -> i32 {
    let step = max(textureLoad(in0, vec2i(0, 0), 0).b, 0.01);
    let at_bar = (params.beats + y / step) / 4.0 + params.drift;
    return i32(floor(at_bar / bars));
}

fn holds_gate(turn: i32) -> bool {
    let every = max(i32(round(params.every)), 1);
    let offset = i32(u32(max(params.seed, 0)) % u32(every));
    return ((turn + offset) % every + every) % every == 0;
}

// Presence: each gate goes at its own share of it; at a steady presence a gate stands or not.
fn wanted(turn: i32) -> bool {
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(turn), u32(max(params.seed, 0)), 83u))) / 4294967296.0;
    return params.presence * 1.6 - r.x >= 1.0 / 16.0;
}

fn ease(x: f32) -> f32 {
    let t = clamp(x, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Turn n's lap in the memory, as red stores it: 1 to 16.
fn lap_of(n: f32) -> f32 {
    let lap = floor(n / SLOTS);
    return lap - 16.0 * floor(lap / 16.0) + 1.0;
}

fn slot_of(n: f32) -> i32 {
    return i32(n - SLOTS * floor(n / SLOTS));
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
    let t = turns();
    // The k-th turn beyond `near` whose number is a multiple of `every`.
    var seen = 0u;
    for (var j = 0u; j < t.found; j++) {
        let y = t.ys[j];
        if y < params.near {
            continue;
        }
        let turn = turn_number(y, t.bars);
        if !holds_gate(turn) {
            continue;
        }
        if seen != k {
            seen += 1u;
            continue;
        }
        let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
        let lane = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
        // How far it stands out of the water: from the memory, or without one at once by presence.
        var up = select(0.0, 1.0, wanted(turn));
        if textureDimensions(in1).x > 1u {
            let n = f32(turn);
            let m = textureLoad(in1, vec2i(0, slot_of(n)), 0);
            if f32(i32(round(m.r)) / 4) == lap_of(n) {
                up = ease(m.g);
            }
        }
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

// One texel of the memory: texel (0, n modulo SLOTS) is the gate at turn n, (1, 0) the clock.
// in1 is the memory of the last frame.
fn memory(cell: vec2u) -> vec4f {
    let clock = textureLoad(in1, vec2i(1, 0), 0);
    let whole_beats = floor(params.beats);
    let wrapped = whole_beats - 1024.0 * floor(whole_beats / 1024.0);
    if cell.x >= 1u {
        return vec4f(wrapped, params.beats - whole_beats, 0.0, 1.0);
    }
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    if lane0.b <= 0.0 {
        return vec4f(0.0);
    }
    // The beats since the last frame. A jump (a seek, the first frame) starts the memory afresh.
    var dt = wrapped - clock.r;
    if dt < -512.0 {
        dt += 1024.0;
    }
    dt += params.beats - whole_beats - clock.g;
    let jump = clock.a < 0.5 || dt < -0.01 || dt > 4.0;
    let t = turns();
    var first = 0u;
    while first < t.found && t.ys[first] < params.near {
        first += 1u;
    }
    if first >= t.found {
        return vec4f(0.0);
    }
    let n_first = f32(turn_number(t.ys[first], t.bars)) - 1.0;
    let slot = f32(cell.y);
    let n = n_first + (slot - n_first - SLOTS * floor((slot - n_first) / SLOTS));
    // Its distance ahead, if it is in view; far ahead if not.
    var y = 1000.0;
    for (var j = first; j < t.found; j++) {
        if f32(turn_number(t.ys[j], t.bars)) == n {
            y = t.ys[j];
            break;
        }
    }
    let want = wanted(i32(n));
    let before = textureLoad(in1, vec2i(cell), 0);
    var by_presence = (i32(round(before.r)) & 1) == 1;
    var risen = before.g;
    if jump || f32(i32(round(before.r)) / 4) != lap_of(n) {
        by_presence = want;
        risen = select(0.0, 1.0, want);
    } else {
        // Within `hold` metres it keeps what it does until it has passed.
        if y > params.hold {
            by_presence = want;
        }
        let rate = dt / (max(params.ease, 0.05) * 4.0);
        risen = clamp(risen + select(-rate, rate, by_presence), 0.0, 1.0);
    }
    return vec4f(select(0.0, 1.0, by_presence) + 4.0 * lap_of(n), risen, 0.0, y);
}

fn kick() -> f32 {
    let b = clamp(params.beat, 0.0, 1.0);
    return smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    if params.what == what_memory {
        return memory(vec2u(v.pos.xy));
    }
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
