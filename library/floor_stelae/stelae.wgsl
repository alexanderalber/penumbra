// Groups of stones on the turns of the valley stage's lane, drawn by geo.tex_instance nodes, one
// per model part (stelae.py writes the models). Render each 5 x 8: texel (m, k) is stone m of the
// group at the k-th turn of the lane ahead of `near` minus 4 m. A turn holds a group with the
// share `groups`, else nothing; a group has 1 to `size` stones. Turn, group, stones and their
// looks follow from the turn's number (bars travelled at the turn, over the bars between turns)
// and `seed`, so a group keeps its place, stones and models as it comes nearer.
// A turn is where the lane stops swinging one way and starts back: where its sideways slope
// changes sign. The group stands on its outside, beyond `clear`, spread along the lane.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the shore (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a turn every `every` beats
// on alternate sides.
// `what` place: where a copy's origin goes (rgb). aim: where its front points. a, c, g, e: the
// scale of that model's body, 0 where another model or none stands; a1, a2, g1, g2: the scale of
// a front with glyph 1 or 2. rim: the light on the bodies' rims. glow: the light in the grooves.
// ring1 to ring4: the light on the disc's four rings, from the middle out.
// memory: what each stone remembers from frame to frame. Render it 6 x 32 and feed it back into
// itself through a tex.feedback into its own in1; the other nodes read it in their in1. A stone
// that presence brings or takes rises or sinks over `ease` bars; within `hold` metres of the
// camera it keeps what it does until it has passed. The tide: every `tide_bars` bars, on the
// downbeat, one stone between 8 and 30 m ahead starts to sink or to rise, over the whole period,
// so about `tide_share` of them lie under water. Texel (m, n modulo 32) holds stone m of the group
// at turn n, texel (5, 0) the clock; the channels as in floor_terraces' terraces.wgsl.
// The stones of a group glow in turn: every `glow_every` beats the next one, from near to far,
// lights up to full glow and fades over `glow_fall` beats; the others keep `base` of it. A stone
// alone keeps a small pulse on each beat.
// @param what choice(place|aim|a|c|g|e|a1|a2|g1|g2|rim|glow|ring1|ring2|ring3|ring4|memory) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the light pulses on its start
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): how strong the pulse is
// @param dawn float = 0 0..1 -- the stage's dawn: the stones catch more of the band's light towards 1
// @param swell float = 0 0..1 -- the stage's swell: the grooves glow brighter and the disc fills with light
// @param warmth float = 0 0..1 -- 0 copper rims and ember grooves, 1 jade
// @param presence float = 1 0..1 -- how many stones stand: towards 0 the groups sink into the water one after another, 0 none
// @param seed int = 0 0..1000 -- other groups, stones and sizes
// @param tall float = 2.6 0.5..6 -- the monolith's height, metres, on average; the other models in proportion
// @param wide float = 1 0.2..3 -- the monolith's width, metres
// @param thick float = 0.55 0.1..2 -- the monolith's depth, metres
// @param light float = 1 0..4 -- brightness of rims and grooves
// @param pulse float = 0.25 0..1 -- how much a stone alone brightens on each beat, times the bass
// @param face float = 30 0..90 -- degrees a stone's front turns from the lane towards the camera, on average
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 1.3 0..8 -- metres between the lane's edge and the nearest stone
// @param near float = -4 -8..10 -- where the stones leave, behind the camera
// @param far float = 36 1..56 -- where they appear
// @param groups float = 0.35 0..1 -- share of the turns with a group
// @param size float = 5 1..5 -- most stones in a group
// @param spread float = 1 0..4 -- metres of water between two stones of a group, on average
// @param scatter float = 2.5 0..6 -- metres the stones of a group fan out away from the lane, the near ones furthest
// @param disc float = 0.6 0..1 -- chance that a stone alone or the middle one of an odd group is the disc
// @param glow_every float = 4 0.25..16 -- beats between two stones of a group lighting up in turn: 1 on each beat, 4 on each bar
// @param glow_fall float = 3 0.1..16 -- beats a lit stone takes to fade
// @param base float = 0.2 0..1 -- share of the glow a stone of a group keeps while it is not lit
// @param every float = 16 2..64 -- beats between two turns while no lane picture is wired
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step
// @param ease float = 2 0.25..8 -- bars a stone takes to rise or sink when presence brings or takes it
// @param hold float = 9 0..30 -- metres ahead within which a stone keeps what presence gave it until it has passed
// @param tide_bars float = 4 0..8 -- bars between two stones starting to sink or rise with the tide: 0 no tide, 1, 2 or 4
// @param tide_share float = 0.25 0..0.6 -- share of the stones in view that the tide keeps under water
// @param tide float = 1 0..1 -- the tide runs while this is 0.6 or more: bound to the arrangement's intensity, it runs in mains and drops

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TEXELS: i32 = 256;
// stelae.py: the foot below the water line, as a share of the height.
const BASE: f32 = 0.05;
// Models: the monolith, the slender stone, the broken one, the disc.
const A: u32 = 0u;
const C: u32 = 1u;
const G: u32 = 2u;
const E: u32 = 3u;
// Per model: height, width and depth against the monolith's; the room it takes along the lane
// in widths.
const TALL = array<f32, 4>(1.0, 1.3, 1.08, 0.88);
const WIDE = array<f32, 4>(1.0, 0.6, 1.0, 1.0);
const THICK = array<f32, 4>(1.0, 0.82, 1.0, 0.9);
const ROOM = array<f32, 4>(1.0, 0.7, 1.9, 1.9);
// How far a group reaches along the lane on either side of its turn, at most.
const REACH: f32 = 6.0;
// The memory: turns it holds, and the clock's column.
const SLOTS: f32 = 32.0;
const CLOCK: u32 = 5u;
// Where the tide picks its stones, metres ahead.
const TIDE_NEAR: f32 = 8.0;
const TIDE_FAR: f32 = 30.0;

fn rand(a: i32, b: u32, k: u32) -> vec3f {
    return vec3f(pn_hash(vec3u(bitcast<u32>(a), b + u32(max(params.seed, 0)) * 977u, k))) / 4294967296.0;
}

fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

struct Turn {
    y: f32,
    // the distance to the turn after it, or before it for the last one in view; 0 if alone
    spacing: f32,
}

// The k-th turn of the lane at or beyond `from`: its distance ahead, or a negative number past
// the last one in view.
fn turn_at(k: u32, start: f32) -> Turn {
    var t: Turn;
    t.y = -100.0;
    t.spacing = 0.0;
    var found = 0u;
    var last = -100.0;
    var before = textureLoad(in0, vec2i(0, 0), 0).g;
    for (var i = 1; i < TEXELS; i++) {
        let now = textureLoad(in0, vec2i(i, 0), 0).g;
        if (before < 0.0) != (now < 0.0) && before != now {
            // Where the slope crosses zero between the two texel centres.
            let f = before / (before - now);
            let y = LANE_NEAR + (f32(i) - 0.5 + f) * (LANE_FAR - LANE_NEAR) / f32(TEXELS);
            if found == k + 1u {
                t.spacing = y - t.y;
                return t;
            }
            if y >= start {
                if found == k {
                    t.y = y;
                    if last > -99.0 {
                        t.spacing = y - last;
                    }
                }
                found += 1u;
            }
            last = y;
        }
        before = now;
    }
    return t;
}

struct Stone {
    at: vec3f,
    aim: vec3f,
    // its scale; x is negative on the left of the lane, so its +x side faces the band there too
    size: vec3f,
    model: u32,
    glyph: u32,
    // its place in the group from near to far, and the group's size
    member: u32,
    count: u32,
    shown: bool,
    // the number of its turn, its height, whether it is one of its group on the shore (not in a
    // gap), whether presence wants it standing, its own random number for the tide
    turn: i32,
    tall: f32,
    filled: bool,
    wanted: bool,
    luck: f32,
}

fn group_size(r: f32) -> u32 {
    let sizes = array<u32, 8>(1u, 2u, 2u, 3u, 3u, 3u, 4u, 5u);
    return min(sizes[min(u32(r * 8.0), 7u)], u32(clamp(params.size, 1.0, 5.0)));
}

fn model_of(n: u32, m: u32, turn: i32) -> u32 {
    let middle = n == 1u || ((n & 1u) == 1u && m == n / 2u);
    let r = rand(turn, m, 71u);
    if middle && rand(turn, 9u, 73u).x < params.disc {
        return E;
    }
    return min(u32(r.x * 3.0), 2u);
}

fn step_now() -> f32 {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    return max(select(params.step, lane0.b, lane0.b > 0.0), 0.01);
}

// The number of a turn of the wired lane: the bars travelled at its middle over the bars between
// two turns. The middle of a turn lies half a bar from a bar line, so the floor is far from a jump.
fn turn_number(t: Turn) -> i32 {
    let step = step_now();
    let bars = max(round(abs(t.spacing) / (4.0 * step)), 1.0);
    let at_bar = (params.beats + t.y / step) / 4.0 + params.drift;
    return i32(floor(at_bar / select(4.0, bars, t.spacing != 0.0)));
}

// Stone m of the group at a turn, at full height (`up` lowers it).
fn stone_on(t: Turn, m: u32) -> Stone {
    var s: Stone;
    s.size = vec3f(0.0);
    s.aim = vec3f(0.0, -1.0, 0.0);
    s.at = vec3f(0.0, 0.0, -50.0);
    s.shown = false;
    s.filled = false;
    s.wanted = false;
    s.model = A;
    s.glyph = 1u;
    s.member = m;
    s.count = 1u;
    s.tall = 0.0;
    s.luck = 0.0;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = step_now();
    var y = t.y;
    var lane = vec4f(0.0, 0.0, step, 1.0);
    var sign: f32;
    var turn: i32;
    if wired {
        let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
        lane = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
        // The outside of the turn: the side the lane has swung to.
        let around = textureSampleLevel(in0, pn_clamp, vec2f(u + 8.0 / (LANE_FAR - LANE_NEAR), 0.5), 0.0);
        sign = select(-1.0, 1.0, lane.r >= around.r);
        turn = turn_number(t);
    } else {
        // Without a lane picture `spacing` carries the turn's number.
        turn = i32(t.spacing);
        sign = select(-1.0, 1.0, (turn & 1) == 0);
    }
    s.turn = turn;
    let g = rand(turn, 0u, 61u);
    if g.x >= params.groups {
        return s;
    }
    let n = group_size(g.y);
    if m >= n {
        return s;
    }
    s.count = n;
    // Lay the group out along the lane from near to far: each stone takes the room of its model
    // and some water after it.
    var total = 0.0;
    var mine = 0.0;
    var room_m = 1.0;
    for (var j = 0u; j < n; j++) {
        let model = model_of(n, j, turn);
        let r = rand(turn, j, 67u);
        let room = ROOM[model] * params.wide * mix(0.85, 1.15, r.y) + params.spread * mix(0.3, 1.7, r.z);
        if j == m {
            mine = total;
            room_m = room;
        }
        total += room;
    }
    s.model = model_of(n, m, turn);
    let r = rand(turn, m, 67u);
    let r2 = rand(turn, m, 79u);
    s.glyph = select(1u, 2u, r2.y >= 0.5);
    let tall = params.tall * TALL[s.model] * mix(0.75, 1.2, r.x);
    var wide = params.wide * WIDE[s.model] * mix(0.85, 1.15, r.y);
    let thick = params.thick * THICK[s.model];
    if s.model == E {
        // The disc stays round.
        wide = tall;
    }
    let along = y - 0.5 * total + mine + 0.5 * room_m;
    // Presence: each group goes at its own share of it, its stones one after another; at a
    // steady presence a stone stands or not.
    s.wanted = params.presence * 1.6 - (0.85 * g.z + 0.15 * r2.x) >= 1.0 / 16.0;
    s.luck = r2.z;
    // Across: the near stones of a group stand further out than the far ones, so the group
    // fans out as seen from the camera instead of hiding behind its nearest stone.
    let fan = select(0.5, 1.0 - f32(m) / f32(max(n, 2u) - 1u), n > 1u);
    let off = params.lane * (1.0 + 0.5 * abs(lane.g)) + params.clear + 0.5 * thick
        + params.scatter * (fan * mix(0.7, 1.1, r2.z) + 0.15 * r.z);
    let turn_face = radians(params.face + (r2.x - 0.5) * 40.0);
    s.aim = vec3f(-sign * cos(turn_face), -sin(turn_face), 0.0);
    s.at = vec3f(lane.r + sign * off, along, 0.0);
    s.tall = tall;
    s.filled = !wired || on_floor(along, step);
    s.size = vec3f(sign * wide, thick, tall);
    return s;
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

struct Held {
    by_presence: bool,
    by_tide: bool,
    // how far it has risen by each, 0 to 1
    risen: f32,
    tide: f32,
    // the texel holds this stone, not one of another lap
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

fn stone(k: u32, m: u32) -> Stone {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    var t: Turn;
    if lane0.b > 0.0 {
        t = turn_at(k, params.near - REACH);
        if t.y < params.near - REACH {
            var s: Stone;
            s.shown = false;
            s.size = vec3f(0.0);
            s.aim = vec3f(0.0, -1.0, 0.0);
            s.at = vec3f(0.0, 0.0, -50.0);
            s.model = A;
            s.glyph = 1u;
            s.member = m;
            s.count = 1u;
            return s;
        }
    } else {
        let step = step_now();
        let gap = max(params.every, 1.0) * step;
        let travel = params.beats * step / gap;
        let part = fract(travel);
        let first = ceil((params.near - REACH) / gap + part);
        t.y = gap * (first + f32(k) - part);
        t.spacing = floor(travel) + first + f32(k);
    }
    var s = stone_on(t, m);
    // How far it stands out of the water: from the memory, or without one at once by presence.
    var up = select(0.0, 1.0, s.wanted);
    if textureDimensions(in1).x > 1u && lane0.b > 0.0 {
        let n = f32(s.turn);
        let h = held(textureLoad(in1, vec2i(i32(m), slot_of(n)), 0), n);
        if h.known {
            up = ease(h.risen) * ease(h.tide);
        }
    }
    s.at.z = -(1.0 + BASE) * s.tall * (1.0 - up);
    let along = s.at.y;
    s.shown = s.tall > 0.0 && s.filled && along <= params.far && along >= params.near && up >= 0.001;
    if !s.shown {
        s.size = vec3f(0.0);
    }
    return s;
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
    for (var c = 0; c < 5; c++) {
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
    for (var c = 0; c < 5; c++) {
        for (var s = 0; s < i32(SLOTS); s++) {
            let t = textureLoad(in1, vec2i(c, s), 0);
            let up = (i32(round(t.r)) & 2) == 2;
            if in_reach(t) && up == sink {
                let key = pn_hash(vec3u(u32(s), u32(c) + u32(max(params.seed, 0)) * 31u, u32(period) * 7919u + 101u)).x;
                if key < best {
                    best = key;
                    pick = vec2i(c, s);
                }
            }
        }
    }
    return pick;
}

// One texel of the memory: texel (m, n modulo SLOTS) is stone m of the group at turn n. in1 is the
// memory of the last frame.
fn memory(cell: vec2u) -> vec4f {
    let clock = textureLoad(in1, vec2i(i32(CLOCK), 0), 0);
    let whole_beats = floor(params.beats);
    let wrapped = whole_beats - 1024.0 * floor(whole_beats / 1024.0);
    let on = tide_on();
    let period_len = tide_period();
    let raw = floor((params.beats / 4.0 + params.drift) / period_len);
    let period = select(-1.0, raw - 1024.0 * floor(raw / 1024.0), on);
    if cell.x >= CLOCK {
        return vec4f(wrapped, params.beats - whole_beats, period, 1.0);
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
    // A new period of the tide begins on its downbeat; not when the tide has just come on.
    let fresh_period = on && !jump && clock.b >= 0.0 && period != clock.b;

    // The turns in view, as stone() finds them: the window of turn numbers starts one before the
    // first, and the turn this texel holds is found among them, if it is in view.
    let first = turn_at(0u, params.near - REACH);
    if first.y < params.near - REACH {
        return vec4f(0.0);
    }
    let n_first = f32(turn_number(first)) - 1.0;
    let slot = f32(cell.y);
    let n = n_first + (slot - n_first - SLOTS * floor((slot - n_first) / SLOTS));
    var t = first;
    var seen = false;
    for (var k = 0u; k < 10u; k++) {
        let c = turn_at(k, params.near - REACH);
        if c.y < params.near - REACH {
            break;
        }
        if f32(turn_number(c)) == n {
            t = c;
            seen = true;
            break;
        }
    }
    var s: Stone;
    if seen {
        s = stone_on(t, cell.x);
    } else {
        // Beyond the lane picture: only its number counts; it lies far ahead.
        var far: Turn;
        far.y = 1000.0;
        far.spacing = 0.0;
        s = stone_on(far, cell.x);
        s.turn = i32(n);
        let g = rand(i32(n), 0u, 61u);
        let r2 = rand(i32(n), cell.x, 79u);
        s.wanted = params.presence * 1.6 - (0.85 * g.z + 0.15 * r2.x) >= 1.0 / 16.0;
        s.luck = r2.z;
        s.filled = false;
        s.at.y = 1000.0;
    }
    let y = s.at.y;
    var h = held(textureLoad(in1, vec2i(cell), 0), n);
    if jump || !h.known {
        // Fresh: as presence wants it, and under water by the tide at its share.
        h.by_presence = s.wanted;
        h.risen = select(0.0, 1.0, s.wanted);
        h.by_tide = !(on && s.luck < params.tide_share);
        h.tide = select(0.0, 1.0, h.by_tide);
    } else {
        if y > params.hold {
            h.by_presence = s.wanted;
        }
        let rate = dt / (max(params.ease, 0.05) * 4.0);
        h.risen = clamp(h.risen + select(-rate, rate, h.by_presence), 0.0, 1.0);
        if fresh_period {
            let pick = tide_pick(period);
            if pick.x == i32(cell.x) && pick.y == i32(cell.y) {
                h.by_tide = !h.by_tide;
            }
        }
        if !on {
            h.by_tide = true;
        }
        let tide_rate = dt / (select(max(params.ease, 0.05), period_len, on) * 4.0);
        h.tide = clamp(h.tide + select(-tide_rate, tide_rate, h.by_tide), 0.0, 1.0);
    }
    let code = select(0.0, 1.0, h.by_presence) + select(0.0, 2.0, h.by_tide) + 4.0 * lap_of(n);
    let standing = h.by_presence && s.filled && s.tall > 0.0;
    return vec4f(code, h.risen, h.tide, select(-1000.0, y, standing));
}

// How lit a stone of a group is now: 1 just after its turn to glow, falling to 0 until its next.
// A stone alone gives -1.
fn turn_glow(s: Stone) -> f32 {
    if s.count < 2u {
        return -1.0;
    }
    let every = max(params.glow_every, 0.25);
    let t = (params.beats + 4.0 * params.drift) / every;
    let count = floor(t);
    let n = i32(s.count);
    let since = f32(((i32(count) - i32(s.member)) % n + n) % n) + (t - count);
    let rise = smoothstep(0.0, 0.1, since);
    return rise * exp(-since * every / max(params.glow_fall, 0.05));
}

fn kick() -> f32 {
    let b = clamp(params.beat, 0.0, 1.0);
    return smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
}

fn scale_if(s: Stone, model: u32, glyph: u32) -> vec4f {
    let ok = s.shown && s.model == model && (glyph == 0u || s.glyph == glyph);
    return vec4f(select(vec3f(0.0), s.size, ok), 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    if params.what == what_memory {
        return memory(vec2u(v.pos.xy));
    }
    let s = stone(u32(v.pos.y), u32(v.pos.x));
    switch params.what {
        case what_place: { return vec4f(s.at, 1.0); }
        case what_aim: { return vec4f(s.aim, 1.0); }
        case what_a: { return scale_if(s, A, 0u); }
        case what_c: { return scale_if(s, C, 0u); }
        case what_g: { return scale_if(s, G, 0u); }
        case what_e: { return scale_if(s, E, 0u); }
        case what_a1: { return scale_if(s, A, 1u); }
        case what_a2: { return scale_if(s, A, 2u); }
        case what_g1: { return scale_if(s, G, 1u); }
        case what_g2: { return scale_if(s, G, 2u); }
        default: {}
    }
    let warm = clamp(params.warmth, 0.0, 1.0);
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let beat = params.pulse * kick() * (0.3 + clamp(params.bass, 0.0, 1.0));
    let lit = turn_glow(s);
    // A stone alone: today's light plus a small pulse on each beat. In a group: the lit one at
    // full glow, the others at `base`.
    let rim_turn = select(mix(0.55, 1.7, lit), 1.0 + beat, lit < 0.0);
    let glow_turn = select(mix(params.base, 3.0, lit), 1.0 + 2.0 * beat, lit < 0.0);
    if params.what == what_rim {
        let tone = mix(vec3f(1.0, 0.5, 0.2), vec3f(0.45, 1.0, 0.75), warm);
        return vec4f(tone * params.light * (0.6 + 0.5 * dawn + 0.5 * swell) * rim_turn, 1.0);
    }
    let ember = mix(vec3f(1.0, 0.42, 0.14), vec3f(0.5, 1.0, 0.78), warm);
    if params.what == what_glow {
        return vec4f(ember * params.light * (0.35 + 0.2 * dawn + 0.9 * swell) * glow_turn, 1.0);
    }
    // The disc's rings fill with light from the middle out as the swell rises, and a ring of light
    // runs out over each beat.
    var ring = 0u;
    switch params.what {
        case what_ring1: { ring = 0u; }
        case what_ring2: { ring = 1u; }
        case what_ring3: { ring = 2u; }
        default: { ring = 3u; }
    }
    let starts = array<f32, 4>(0.0, 0.225, 0.52, 0.76);
    let middles = array<f32, 4>(0.11, 0.37, 0.64, 0.88);
    let reach = 1.05 * swell;
    let fill = smoothstep(starts[ring], starts[ring] + 0.2, reach);
    let b = clamp(params.beat, 0.0, 1.0);
    let wave = (1.0 - smoothstep(0.0, 0.22, abs(middles[ring] - b))) * (1.0 - b);
    let level = 0.07 + 0.1 * dawn + 1.1 * fill + params.pulse * 1.6 * wave * (0.3 + clamp(params.bass, 0.0, 1.0));
    return vec4f(ember * params.light * level * select(mix(params.base, 2.2, lit), 1.0, lit < 0.0), 1.0);
}
