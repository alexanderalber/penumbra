// Plants at the water's edge beside the lane of the valley stage. A stretch of a shore belongs to
// one kind: reeds, cattails, sedge, papyrus or agave; an agave now and then carries a tall mast.
// With `front` at 1 this draws the low tufts instead, a layer between those plants and the lane.
// One texel per clump: a column is a line of clumps along the lane (left lines first, the line
// at the lane's edge then the one behind it, then the right ones), a row a clump along it. The
// clumps stand fixed to the floor, a few left out at random. They sway across the lane, a wave
// running along the line towards the camera, wider with the bass. Their tips glint in the swell.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the shore (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// `what` place: the clump's foot (rgb). aim: its turn and lean. scale: its size (0 where none
// stands), alpha its kind (0 to 4 as in kind_of, 5 tufts, plus 10 when it carries a mast); pick.wgsl
// makes one model's scale of it. color: the glint, as a factor on the plants' ramp. Render each
// 4 x 72, with the memory in in1. memory: what each clump remembers from frame to frame. Render it
// 5 x 128 and feed it back into itself through a tex.feedback into its own in1. A clump that
// presence brings or takes grows or shrinks over `ease` bars; within `hold` metres of the camera
// it keeps what it does until it has passed. Texel (column, n modulo 128) holds clump n of that
// column: red 1 if it stands by presence plus 4 times its lap, green how far it has grown; texel
// (4, 0) the clock (whole beats modulo 1024, the beat's fraction).
// @param what choice(place|aim|scale|color|memory) = place -- what this node writes
// @param front float = 0 0..1 -- 1: the low tufts in front; 0: the stretches of plants
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): the plants sway wider with it
// @param swell float = 0 0..1 -- the stage's swell: the tips glint with it
// @param presence float = 1 0..1 -- how many plants stand: towards 0 the clumps shrink into the water one after another, the tufts first, 0 none
// @param spacing float = 0.55 0.2..3 -- metres between two places of a line; wide plants take every second place
// @param fill float = 0.8 0..1 -- share of the places that hold a clump
// @param size float = 1.2 0.3..3 -- the reeds' height, metres, on average; the other kinds follow (the camera flies 0.9 to 2.4 m up)
// @param sway float = 7 0..30 -- degrees the reeds lean to either side at full bass; stiffer kinds lean less, an agave not at all
// @param calm float = 0.3 0..1 -- share of the sway left when the bass is quiet
// @param wave float = 6 1..40 -- metres between two crests of the sway along a line
// @param glint float = 3 0..10 -- how bright the tips glint at full swell
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param edge float = 0.2 0..4 -- metres between the lane's edge and the first line
// @param depth float = 0.7 0.1..4 -- metres between the first line and the second
// @param near float = -3 -8..10 -- where the plants leave, behind the camera
// @param far float = 30 1..56 -- where they appear
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step
// @param ease float = 2 0.25..8 -- bars a clump takes to grow or shrink when presence brings or takes it
// @param hold float = 9 0..30 -- metres ahead within which a clump keeps what presence gave it until it has passed
// @param stretch float = 9 1..60 -- metres of shore that belong to one kind, on average
// @param share_reeds float = 1 0..1 -- how often a stretch of reeds comes, against the others; 0 never
// @param share_cattails float = 1 0..1 -- the same for cattails
// @param share_sedge float = 1 0..1 -- the same for sedge
// @param share_papyrus float = 1 0..1 -- the same for papyrus
// @param share_agave float = 1 0..1 -- the same for agaves
// @param mast float = 0.08 0..1 -- share of the agaves that carry a mast
// @param tufts float = 0.6 0..1 -- share of the places in front that hold a tuft
// @param seed float = 0 0..1000 -- another order of the stretches

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// The memory: clumps per column it holds, and the clock's column.
const SLOTS: f32 = 128.0;
const CLOCK: u32 = 4u;

// The kinds, in the order of pick.wgsl's `kind`, then the tufts. size: on `size`. vary: spread of
// the clump's size. sway: on `sway`. edge: metres on `edge`. depth: on `depth`. every: places per
// clump. fill: on `fill`. turn: degrees of random turn either way. Reeds and cattails turn little,
// as their bend reads across the lane; an agave is round, any turn does.
struct Kind {
    size: f32,
    vary: f32,
    sway: f32,
    edge: f32,
    depth: f32,
    every: i32,
    fill: f32,
    turn: f32,
}

const KINDS: u32 = 5u;
const TUFTS: u32 = 5u;
const MAST: f32 = 10.0;
const FRONT_SPACING: f32 = 0.73;

fn kind_of(k: u32) -> Kind {
    switch k {
        case 0u: { return Kind(1.0, 0.3, 1.0, 0.15, 1.0, 1, 1.0, 0.0); }
        case 1u: { return Kind(1.25, 0.3, 0.7, 0.15, 1.0, 1, 1.0, 25.0); }
        case 2u: { return Kind(0.85, 0.3, 0.85, 0.2, 1.3, 2, 1.1, 40.0); }
        case 3u: { return Kind(1.65, 0.2, 0.55, 0.3, 1.3, 2, 1.2, 40.0); }
        case 4u: { return Kind(0.7, 0.3, 0.0, 0.25, 1.4, 2, 0.9, 180.0); }
        default: { return Kind(0.32, 0.35, 0.7, -0.25, 0.43, 1, 1.0, 40.0); }
    }
}

fn share_of(k: u32) -> f32 {
    switch k {
        case 0u: { return params.share_reeds; }
        case 1u: { return params.share_cattails; }
        case 2u: { return params.share_sedge; }
        case 3u: { return params.share_papyrus; }
        default: { return params.share_agave; }
    }
}

fn is_front() -> bool {
    return params.front > 0.5;
}

fn spacing() -> f32 {
    return max(params.spacing, 0.05) * select(1.0, FRONT_SPACING, is_front());
}

fn hash01(a: i32, b: u32, c: u32) -> f32 {
    return f32(pn_hash(vec3u(bitcast<u32>(a), b, c)).x) / 4294967296.0;
}

// The kind of stretch number p of a side. Each stretch steps on from the one before it by a random
// 1 to 4 of the 5 kinds, so two stretches in a row differ; every 8 stretches start afresh, so no
// clump has to count far back. A kind whose share says no gives way to the next.
fn stretch_kind(p: i32, side: u32) -> u32 {
    let s = side + 2u * u32(max(params.seed, 0.0));
    let block = i32(floor(f32(p) / 8.0));
    var k = pn_hash(vec3u(bitcast<u32>(block), s, 97u)).x % KINDS;
    for (var q = block * 8 + 1; q <= p; q++) {
        k = (k + 1u + pn_hash(vec3u(bitcast<u32>(q), s, 101u)).x % (KINDS - 1u)) % KINDS;
    }
    for (var t = 0u; t < KINDS; t++) {
        if hash01(p, s + k * 7u, 103u) < share_of(k) {
            return k;
        }
        k = (k + 1u) % KINDS;
    }
    return KINDS;
}

fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

fn ease(x: f32) -> f32 {
    let t = clamp(x, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Clump n's lap in the memory, as red stores it: 1 to 16.
fn lap_of(n: f32) -> f32 {
    let lap = floor(n / SLOTS);
    return lap - 16.0 * floor(lap / 16.0) + 1.0;
}

fn slot_of(n: f32) -> i32 {
    return i32(n - SLOTS * floor(n / SLOTS));
}

struct Travel {
    whole: f32,
    part: f32,
    // the first clump in view, counted from `whole`
    first: f32,
}

// Clump n of a line stands at y = spacing * n - the metres travelled; the whole and the fraction
// of that count are kept apart to stay exact far into a file.
fn travel_of(line: u32) -> Travel {
    var t: Travel;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let step = select(params.step, lane0.b, lane0.b > 0.0);
    let gap = spacing();
    let travel = params.beats * step / gap + 0.37 * f32(line);
    t.whole = floor(travel);
    t.part = travel - t.whole;
    t.first = ceil(params.near / gap + t.part);
    return t;
}

// Whether presence wants clump n of a column standing: each clump at its own share of it; at a
// steady presence it stands or not.
fn wanted(n: i32, column: u32) -> bool {
    let w = f32(pn_hash(vec3u(bitcast<u32>(n), column, 73u)).x) / 4294967296.0;
    // The tufts leave first and come back last, so they never stand there alone.
    return params.presence * 1.6 - w >= 1.0 / 16.0 + select(0.0, 0.35, is_front());
}

// One texel of the memory. in1 is the memory of the last frame.
fn memory(cell: vec2u) -> vec4f {
    let clock = textureLoad(in1, vec2i(i32(CLOCK), 0), 0);
    let whole_beats = floor(params.beats);
    let wrapped = whole_beats - 1024.0 * floor(whole_beats / 1024.0);
    if cell.x >= CLOCK {
        return vec4f(wrapped, params.beats - whole_beats, 0.0, 1.0);
    }
    // The beats since the last frame. A jump (a seek, the first frame) starts the memory afresh.
    var dt = wrapped - clock.r;
    if dt < -512.0 {
        dt += 1024.0;
    }
    dt += params.beats - whole_beats - clock.g;
    let jump = clock.a < 0.5 || dt < -0.01 || dt > 4.0;
    let t = travel_of(cell.x % 2u);
    let n_first = t.whole + t.first - 1.0;
    let slot = f32(cell.y);
    let n = n_first + (slot - n_first - SLOTS * floor((slot - n_first) / SLOTS));
    let y = spacing() * (n - t.whole - t.part);
    let want = wanted(i32(n), cell.x);
    let before = textureLoad(in1, vec2i(cell), 0);
    var by_presence = (i32(round(before.r)) & 1) == 1;
    var risen = before.g;
    if jump || f32(i32(round(before.r)) / 4) != lap_of(n) {
        by_presence = want;
        risen = select(0.0, 1.0, want);
    } else {
        if y > params.hold {
            by_presence = want;
        }
        let bpb = 4.0;
        let rate = dt / (max(params.ease, 0.05) * bpb);
        risen = clamp(risen + select(-rate, rate, by_presence), 0.0, 1.0);
    }
    return vec4f(select(0.0, 1.0, by_presence) + 4.0 * lap_of(n), risen, 0.0, y);
}

struct Clump {
    at: vec3f,
    aim: vec3f,
    size: vec3f,
    phase: f32,
    kind: f32,
}

fn clump(cell: vec2u) -> Clump {
    var c: Clump;
    c.size = vec3f(0.0);
    c.aim = vec3f(1.0, 0.0, 0.0);
    c.phase = 0.0;
    c.kind = 0.0;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let side = cell.x / 2u;
    let line = cell.x % 2u;
    if side > 1u {
        return c;
    }
    let gap = spacing();
    let tr = travel_of(line);
    let part = tr.part;
    let k = tr.first + f32(cell.y);
    let n = i32(tr.whole + k);
    let r = vec4f(vec3f(pn_hash(vec3u(bitcast<u32>(n), cell.x, 71u))), f32(pn_hash(vec3u(bitcast<u32>(n), cell.x, 73u)).x)) / 4294967296.0;
    let y = gap * (k - part + 0.6 * (r.x - 0.5));
    // The stretch by the clump's place on the floor, its border a little frayed.
    let r2 = vec3f(pn_hash(vec3u(bitcast<u32>(n), cell.x, 79u))) / 4294967296.0;
    let which = select(stretch_kind(i32(floor(gap * f32(n) / max(params.stretch, 1.0) + 0.3 * (r2.x - 0.5))), side), TUFTS, is_front());
    if which >= KINDS && !is_front() {
        return c;
    }
    let kind = kind_of(which);
    c.kind = f32(which);
    if r2.z < params.mast && which == 4u {
        c.kind += MAST;
    }
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let sign = select(-1.0, 1.0, side == 1u);
    let depth = params.depth * kind.depth;
    let off = params.lane * (1.0 + 0.5 * abs(lane.g)) + params.edge + kind.edge + f32(line) * depth + 0.45 * depth * r.y;
    c.phase = r.z;
    // The sway: a lean across the lane, a wave running along the line towards the camera, one
    // crest every `wave` metres; each clump a little out of step.
    let strength = mix(clamp(params.calm, 0.0, 1.0), 1.0, clamp(params.bass, 0.0, 1.0));
    let along = y + params.beats * step;
    let wave = sin(6.2831853 * (along / max(params.wave, 0.5) + 0.25 * params.beats + 0.3 * r.z));
    let lean = radians(params.sway * kind.sway) * strength * wave + radians(4.0) * (r.w - 0.5);
    let yaw = radians(kind.turn * (2.0 * r2.y - 1.0));
    // aim: the clump's +y points across the lane, turned by `yaw` and tipped down by `lean`, so
    // its +z leans the other way by as much.
    c.aim = vec3f(cos(lean) * cos(yaw), cos(lean) * sin(yaw), -sin(lean));
    // Presence: how far it stands, from the memory, or without one at once.
    var up = select(0.0, 1.0, wanted(n, cell.x));
    if textureDimensions(in1).x > 1u {
        let nf = tr.whole + k;
        let m = textureLoad(in1, vec2i(i32(cell.x), slot_of(nf)), 0);
        if f32(i32(round(m.r)) / 4) == lap_of(nf) {
            up = ease(m.g);
        }
    }
    c.at = vec3f(lane.r + sign * off, y, 0.0);
    let share = select(params.fill * kind.fill, params.tufts, is_front());
    let there = r.y * 0.999 + 0.0005 < share && (kind.every == 1 || (n & 1) == 0);
    let far = select(params.far, min(params.far, 22.0), is_front());
    if y > far || up < 0.02 || !there || (wired && !on_floor(y, step)) {
        return c;
    }
    let h = params.size * kind.size * mix(1.0 - kind.vary, 1.0 + kind.vary, r.x) * up;
    // Every other clump mirrored, so the clumps do not all look alike.
    let flip = select(1.0, -1.0, r.z < 0.5);
    c.size = vec3f(flip * h, h, h);
    return c;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    if params.what == what_memory {
        return memory(vec2u(v.pos.xy));
    }
    let c = clump(vec2u(v.pos.xy));
    if params.what == what_place {
        return vec4f(c.at, 1.0);
    }
    if params.what == what_aim {
        return vec4f(c.aim, 1.0);
    }
    if params.what == what_scale {
        return vec4f(c.size, c.kind);
    }
    // The glint: faint without the swell, up to `glint` times the ramp's tip in it, each clump
    // catching the light at its own moment of the sway.
    let swell = clamp(params.swell, 0.0, 1.0);
    let flicker = 0.55 + 0.45 * sin(6.2831853 * (0.5 * params.beats + c.phase));
    return vec4f(vec3f(0.35 + params.glint * swell * flicker), 1.0);
}
