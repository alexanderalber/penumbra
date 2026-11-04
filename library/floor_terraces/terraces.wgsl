// Terraces on the shore beside the lane of the valley stage, drawn by two geo.tex_instance nodes:
// one with the terrace model, one with the pyramid model (terraces.py). One texel per place: a
// column of texels is a line of places along the shore (left lines first, near line then back
// line, then the right ones), a row a place along it. Each place holds a terrace, or on the back
// line now and then a stepped pyramid instead. The places stand `spacing` bars of travel apart,
// fixed to the floor, so they pass with it; the back line stands half a place later.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the shore (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// `what` place: where the model's origin goes (rgb). aim: where its +y points (towards the lane).
// terrace, pyramid: the scale of that model, 0 where the place holds the other one or nothing.
// color: the rim light's colour. Render each 4 x 12, with the memory in in1.
// memory: what each place remembers from frame to frame. Render it 5 x 64 and feed it back into
// itself through a tex.feedback into its own in1. A terrace that presence brings or takes rises
// or sinks over `ease` bars; within `hold` metres of the camera it keeps what it does until it has
// passed, so a stretch of terraces comes in from the horizon and leaves behind the camera. The
// tide: every `tide_bars` bars, on the downbeat, one terrace between 8 and 30 m ahead starts to
// sink or to rise, over the whole period, so about `tide_share` of them lie under water.
// Texel (column, n modulo 64) holds place n of that column, texel (4, 0) the clock:
//   red    1 if it stands by presence, plus 2 if it stands by the tide, plus 4 times its lap (the
//          place number over 64, modulo 16, plus 1); the clock: whole beats modulo 1024
//   green  how far it has risen by presence, 0 to 1; the clock: the beat's fraction
//   blue   how far it has risen by the tide, 0 to 1; the clock: the tide's period, -1 while off
//   alpha  metres ahead while it stands by presence, else -1000; the clock: 1
// @param what choice(place|aim|terrace|pyramid|color|memory) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the rims pulse on its start
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): lifts the rim light
// @param dawn float = 0 0..1 -- the stage's dawn: the rims catch more of the band's light towards 1
// @param warmth float = 0 0..1 -- 0 copper rims, 1 a cooler pale bronze
// @param presence float = 1 0..1 -- how many terraces stand: towards 0 they sink into the water one after another, 0 none
// @param spacing float = 3 0.5..8 -- bars of travel between two places of a line
// @param fill float = 0.75 0..1 -- share of the places that hold a terrace or a pyramid; water between the others
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param tall float = 0.9 0.3..6 -- the near line's height, metres, on average (the camera flies 0.9 to 2.4 m up)
// @param back float = 2.4 0.5..6 -- the back line's height as a multiple of `tall`
// @param depth float = 2.4 0.5..8 -- how far a terrace reaches back from its front, metres
// @param pyramids float = 0.2 0..1 -- share of the back line's places that hold a pyramid
// @param pyramid float = 0.75 0.3..2 -- a pyramid's size: 1 is as wide as its place along the shore, about as high
// @param face float = 25 0..90 -- degrees a pyramid's stair turns from the lane towards the camera
// @param rim float = 1 0..4 -- brightness of the rim light
// @param pulse float = 0.3 0..1 -- how much the rims brighten on each beat, times the bass
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 2.2 0..8 -- metres between the lane's edge and the near line's front
// @param near float = -4 -8..10 -- where the terraces leave, behind the camera
// @param far float = 36 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step
// @param ease float = 2 0.25..8 -- bars a terrace takes to rise or sink when presence brings or takes it
// @param hold float = 9 0..30 -- metres ahead within which a terrace keeps what presence gave it until it has passed
// @param tide_bars float = 2 0..8 -- bars between two terraces starting to sink or rise with the tide: 0 no tide, 1, 2 or 4
// @param tide_share float = 0.25 0..0.6 -- share of the terraces in view that the tide keeps under water
// @param tide float = 1 0..1 -- the tide runs while this is 0.6 or more: bound to the arrangement's intensity, it runs in mains and drops

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// terraces.py: the base below the water line, as a share of the height.
const BASE: f32 = 0.05;
// The memory: places per column it holds, and the clock's column.
const SLOTS: f32 = 64.0;
const CLOCK: u32 = 4u;
// Where the tide picks its terraces, metres ahead.
const TIDE_NEAR: f32 = 8.0;
const TIDE_FAR: f32 = 30.0;

// Whether the shore stands under a thing `y` metres ahead: the lane picture's alpha, read as in
// or out at the middle of the thing's beat cell, so it stays in or out as it comes nearer.
fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

struct Place {
    at: vec3f,
    aim: vec3f,
    // along, depth, height; 0 where nothing stands
    size: vec3f,
    pyramid: bool,
}

// A place of a line: where it stands and what it holds, at full height.
struct Site {
    p: Place,
    y: f32,
    // the place's number along its column
    n: f32,
    // it holds a terrace or a pyramid on the shore (not water, not in a gap)
    filled: bool,
    // presence wants it standing
    wanted: bool,
    height: f32,
    // its own random number for the tide
    luck: f32,
}

struct Travel {
    whole: f32,
    part: f32,
    // the first place in view, counted from `whole`
    first: f32,
}

fn step_now() -> f32 {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    return select(params.step, lane0.b, lane0.b > 0.0);
}

fn place_gap() -> f32 {
    return max(params.spacing, 0.1) * max(params.beats_per_bar, 1.0) * step_now();
}

// How far a column has travelled, in places. The whole and the fraction are kept apart to stay
// exact far into a file.
fn travel_of(column: u32) -> Travel {
    var t: Travel;
    let travel = (params.beats / max(params.beats_per_bar, 1.0) + params.drift) / max(params.spacing, 0.1);
    t.whole = floor(travel);
    t.part = travel - t.whole;
    t.first = ceil(params.near / place_gap() + t.part - 0.5 * f32(column % 2u));
    return t;
}

// Place `k` of a column, counted from the column's whole travel.
fn site(column: u32, k: f32) -> Site {
    var q: Site;
    var p: Place;
    p.pyramid = false;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = step_now();
    let side = column / 2u;
    let line = column % 2u;
    // Place n of a line stands at y = gap_y * (n - travel); the back line half a place later.
    let gap_y = place_gap();
    let t = travel_of(column);
    let y = gap_y * (k - t.part + 0.5 * f32(line));
    let n = i32(t.whole + k);
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(n), column, 53u))) / 4294967296.0;
    let r2 = vec3f(pn_hash(vec3u(bitcast<u32>(n), column, 59u))) / 4294967296.0;
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, side == 1u);
    let edge = params.lane * (1.0 + 0.5 * abs(lane.g)) + params.clear;
    let d0 = params.depth;
    var along: f32;
    var depth: f32;
    var height: f32;
    var off: f32;
    if line == 0u {
        along = gap_y * mix(0.55, 0.9, r.x);
        depth = d0 * mix(0.8, 1.2, r2.x);
        height = params.tall * mix(0.6, 1.25, r.y);
        off = edge + 0.5 * depth;
    } else {
        p.pyramid = r2.y < params.pyramids;
        depth = d0 * mix(1.1, 1.6, r2.x);
        off = edge + 1.2 * d0 + 0.6 + 0.5 * depth;
        if p.pyramid {
            along = gap_y * 0.9 * params.pyramid;
            depth = along;
            height = along * mix(0.85, 1.05, r.y);
            off = edge + 1.2 * d0 + 0.6 + 0.5 * along;
        } else {
            along = gap_y * mix(0.6, 0.95, r.x);
            height = params.tall * params.back * mix(0.7, 1.2, r.y);
        }
    }
    // Facing the lane; a pyramid's stair turned towards the camera.
    let turn = select(0.0, radians(params.face), p.pyramid);
    p.aim = vec3f(-sign * cos(turn), -sin(turn), 0.0);
    p.at = vec3f(lane.r + sign * off, y, 0.0);
    p.size = vec3f(along, depth, height);
    q.p = p;
    q.y = y;
    q.n = t.whole + k;
    q.height = height;
    q.filled = r2.z < params.fill && (!wired || on_floor(y, step));
    // Presence: each terrace has its own share of it, so as presence falls they go one after
    // another. At a steady presence a terrace stands or not.
    q.wanted = params.presence * 1.6 - r.z >= 1.0 / 16.0;
    q.luck = r.x;
    return q;
}

fn ease(x: f32) -> f32 {
    let t = clamp(x, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Place n's lap in the memory, as red stores it: 1 to 16.
fn lap_of(n: f32) -> f32 {
    let lap = floor(n / SLOTS);
    return lap - 16.0 * floor(lap / 16.0) + 1.0;
}

fn slot_of(n: f32) -> i32 {
    return i32(n - SLOTS * floor(n / SLOTS));
}

// The place number a memory texel holds: the one in [n_first, n_first + SLOTS) that is `slot`
// modulo SLOTS.
fn number_in(slot: f32, n_first: f32) -> f32 {
    return n_first + (slot - n_first - SLOTS * floor((slot - n_first) / SLOTS));
}

struct Held {
    by_presence: bool,
    by_tide: bool,
    // how far it has risen by each, 0 to 1
    risen: f32,
    tide: f32,
    // the texel holds this place, not one of another lap
    known: bool,
}

fn held(t: vec4f, n: f32) -> Held {
    var h: Held;
    let code = i32(round(t.r));
    h.known = f32(code / 4) == lap_of(n);
    h.by_presence = (code & 1) == 1;
    h.by_tide = (code & 2) == 2;
    h.risen = t.g;
    h.tide = t.b;
    return h;
}

fn tide_period() -> f32 {
    return max(round(params.tide_bars), 1.0);
}

fn tide_on() -> bool {
    return params.tide >= 0.6 && params.tide_bars >= 0.5;
}

// Whether a texel of last frame's memory (in1) is one the tide may pick: it stands by presence
// between TIDE_NEAR and TIDE_FAR.
fn in_reach(t: vec4f) -> bool {
    return (i32(round(t.r)) & 1) == 1 && t.a >= TIDE_NEAR && t.a <= TIDE_FAR;
}

// The tide's pick for the period starting now, as a memory texel, or -1. Every texel reaches the
// same answer from last frame's memory. Fewer under water than the share: one sinks; more: one
// rises; as many: they take turns, period by period.
fn tide_pick(period: f32) -> vec2i {
    var count = 0;
    var down = 0;
    for (var c = 0; c < 4; c++) {
        for (var s = 0; s < i32(SLOTS); s++) {
            let t = textureLoad(in1, vec2i(c, s), 0);
            if in_reach(t) {
                count += 1;
                down += select(1, 0, (i32(round(t.r)) & 2) == 2);
            }
        }
    }
    if count == 0 {
        return vec2i(-1);
    }
    let aim = i32(round(clamp(params.tide_share, 0.0, 1.0) * f32(count)));
    var sink = down < aim || (down == aim && (i32(period) & 1) == 0);
    if down == 0 {
        sink = true;
    }
    if down >= count {
        sink = false;
    }
    var best = 0xffffffffu;
    var pick = vec2i(-1);
    for (var c = 0; c < 4; c++) {
        for (var s = 0; s < i32(SLOTS); s++) {
            let t = textureLoad(in1, vec2i(c, s), 0);
            let up = (i32(round(t.r)) & 2) == 2;
            if in_reach(t) && up == sink {
                let key = pn_hash(vec3u(u32(s), u32(c), u32(period) * 7919u + 101u)).x;
                if key < best {
                    best = key;
                    pick = vec2i(c, s);
                }
            }
        }
    }
    return pick;
}

// One texel of the memory. in1 is the memory of the last frame.
fn memory(cell: vec2u) -> vec4f {
    let clock = textureLoad(in1, vec2i(i32(CLOCK), 0), 0);
    let bpb = max(params.beats_per_bar, 1.0);
    let whole_beats = floor(params.beats);
    let wrapped = whole_beats - 1024.0 * floor(whole_beats / 1024.0);
    let on = tide_on();
    let period_len = tide_period();
    let raw = floor((params.beats / bpb + params.drift) / period_len);
    let period = select(-1.0, raw - 1024.0 * floor(raw / 1024.0), on);
    if cell.x >= CLOCK {
        return vec4f(wrapped, params.beats - whole_beats, period, 1.0);
    }
    // The beats since the last frame. A jump (a seek, the first frame) starts the memory afresh.
    var dt = wrapped - clock.r;
    if dt < -512.0 {
        dt += 1024.0;
    }
    dt += params.beats - whole_beats - clock.g;
    let jump = clock.a < 0.5 || dt < -0.01 || dt > 4.0;
    // A new period of the tide begins on its downbeat; not when the tide has just come on.
    let fresh_period = on && !jump && clock.b >= 0.0 && period != clock.b;

    let column = cell.x;
    let tr = travel_of(column);
    let n = number_in(f32(cell.y), tr.whole + tr.first - 1.0);
    let q = site(column, n - tr.whole);
    var h = held(textureLoad(in1, vec2i(cell), 0), n);
    if jump || !h.known {
        // Fresh: as presence wants it, and under water by the tide at its share.
        h.by_presence = q.wanted;
        h.risen = select(0.0, 1.0, q.wanted);
        h.by_tide = !(on && q.luck < params.tide_share);
        h.tide = select(0.0, 1.0, h.by_tide);
    } else {
        if q.y > params.hold {
            h.by_presence = q.wanted;
        }
        let rate = dt / (max(params.ease, 0.05) * bpb);
        h.risen = clamp(h.risen + select(-rate, rate, h.by_presence), 0.0, 1.0);
        if fresh_period {
            let pick = tide_pick(period);
            if pick.x == i32(column) && pick.y == i32(cell.y) {
                h.by_tide = !h.by_tide;
            }
        }
        if !on {
            h.by_tide = true;
        }
        let tide_rate = dt / (select(max(params.ease, 0.05), period_len, on) * bpb);
        h.tide = clamp(h.tide + select(-tide_rate, tide_rate, h.by_tide), 0.0, 1.0);
    }
    let code = select(0.0, 1.0, h.by_presence) + select(0.0, 2.0, h.by_tide) + 4.0 * lap_of(n);
    return vec4f(code, h.risen, h.tide, select(-1000.0, q.y, h.by_presence && q.filled));
}

fn place(cell: vec2u) -> Place {
    var p: Place;
    p.at = vec3f(0.0);
    p.aim = vec3f(0.0, 1.0, 0.0);
    p.size = vec3f(0.0);
    p.pyramid = false;
    if cell.x > 3u {
        return p;
    }
    let tr = travel_of(cell.x);
    let q = site(cell.x, tr.first + f32(cell.y));
    p = q.p;
    // How far it stands out of the water: from the memory, or without one at once by presence.
    var up = select(0.0, 1.0, q.wanted);
    if textureDimensions(in1).x > 1u {
        let h = held(textureLoad(in1, vec2i(i32(cell.x), slot_of(q.n)), 0), q.n);
        if h.known {
            up = ease(h.risen) * ease(h.tide);
        }
    }
    p.at.z = -(1.0 + BASE) * q.height * (1.0 - up);
    if q.y > params.far || up < 0.001 || !q.filled {
        p.size = vec3f(0.0);
    }
    return p;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if params.what == what_memory {
        return memory(cell);
    }
    let p = place(cell);
    if params.what == what_place {
        return vec4f(p.at, 1.0);
    }
    if params.what == what_aim {
        return vec4f(p.aim, 1.0);
    }
    if params.what == what_terrace {
        return vec4f(select(p.size, vec3f(0.0), p.pyramid), 1.0);
    }
    if params.what == what_pyramid {
        return vec4f(select(vec3f(0.0), p.size, p.pyramid), 1.0);
    }
    // The rims: copper, brighter with the bass and the dawn, a small pulse on each beat.
    let b = clamp(params.beat, 0.0, 1.0);
    let kick = smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
    let bass = clamp(params.bass, 0.0, 1.0);
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let level = params.rim * (0.22 + 0.2 * dawn + 0.5 * bass + params.pulse * kick * (0.3 + bass));
    let copper = mix(vec3f(0.55, 0.19, 0.055), vec3f(0.30, 0.22, 0.15), clamp(params.warmth, 0.0, 1.0));
    return vec4f(copper * level, 1.0);
}
