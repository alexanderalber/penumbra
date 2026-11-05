// The rings of mid_glyphs, one short segment per texel, for geo.tex_instance with a thin prism one
// unit long along y (segment.obj). Seven rings hang in the air ahead over the lane, one inside
// the other: plain circles, a ring of short ticks and two step-fret rings (a staircase up, a run
// along the top, a drop and a hook inside, cell after cell around the ring). Each ring turns
// slowly in its own plane, every second one the other way.
// The rings open in a quiet stretch and close on the drop. Opening, they move apart: each grows,
// steps forwards or back and tilts about an axis of its own, so the flat glyph unfolds into a
// cage of rings. The stage's swell opens them; a drop (`snap`, the grid's drop through a filter)
// shuts them within a beat or so. They are lit from behind by the band: brightest where a ring
// crosses the horizon, brighter as they open and towards the dawn. Each beat adds a small pulse.
// in0: the stage's lane picture (its `lane` output); the rings hang over the lane. Unconnected:
// a straight lane at x = 0.
// `what` place: the middle of each segment. aim: its direction. scale: `thickness` across, its
// length along y (0 for none). color: its light. Render it 32 x 16.
// The rings arrive as an event. Presence rising starts it: on each downbeat one more ring is drawn
// on from the inside out, by a bright ember head that runs once round and leaves the line behind
// it. The head fades to the ring's rim light when it closes. Presence falling folds the rings flat
// and dims them, from the outside in. Opening and closing go on through both: they move whatever
// is drawn so far.
// memory: what each ring remembers from frame to frame. Render it 9 x 1 and feed it back into itself
// through a tex.feedback into its own in1; the other nodes read it in their in1. Texel k of the
// seven rings: red how far it is drawn, green how far it is folded, blue its head's light. Texel 7
// the clock: whole bars modulo 1024, the bar's fraction, the step of the arrival modulo 1024, alpha
// 1 once it runs. Texel 8: red the last presence, green the ring being drawn plus 1, blue 1 in the
// frame after a jump, else 2 while the rings are wanted, alpha the light that follows the last ring's closing. A jump in the bars (a
// seek, the first frame, the rings not cooked for a while) forgets all of it. One frame later the
// memory knows whether presence moves: rising or low means an arrival is still to come, anything
// else shows the rings whole, at once.
// @param what choice(place|aim|scale|color|memory) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the rings turn with it
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the light pulses on its start
// @param swell float = 0 0..1 -- the stage's swell: opens the rings
// @param snap float = 0 0..1 -- the drop, held for a few seconds: closes the rings and lights them a little
// @param dawn float = 0 0..1 -- the stage's dawn: more light towards 1
// @param eye float = 1.2 0..10 -- the camera's height over the water, metres (height of the stage's pose)
// @param reach float = 40 1..200 -- metres ahead where the horizon lies (far of the stage's pose)
// @param presence float = 1 0..1 -- rising past 0.25 the rings arrive, falling past 0.5 they leave
// @param bar_index float = 0 0..100000 -- the grid's bar count (bar_index of a chan.beat_grid)
// @param bar float = 0 0..1 -- how far the current bar has gone (bar of a chan.beat_grid)
// @param arrive_bars float = 8 1..32 -- bars an arrival takes: a ring per eighth of it, the last eighth to settle
// @param leave_bars float = 2 0.25..16 -- bars the rings take to fold away
// @param open float = 0 0..1 -- opens the rings at least this far, whatever the swell
// @param open_at float = 0.15 0..1 -- swell where the rings begin to open
// @param open_full float = 0.6 0..1 -- swell where they are fully open
// @param size float = 1 0.2..4 -- the rings' size: 1 is 3.2 m across the outer ring
// @param at float = 9 4..20 -- metres ahead of the camera
// @param hover float = 2.2 0.5..5 -- metres above the water of the rings' middle
// @param follow float = 0.7 0..1 -- how far the rings follow the lane's swing to the sides
// @param spread float = 0.45 0..2 -- how much the rings grow apart when open
// @param depth float = 0.8 0..4 -- metres between two rings along the view when open
// @param tilt float = 50 0..90 -- degrees the rings tilt when open
// @param turn float = 96 8..2000 -- beats the innermost ring takes to turn once; the outer ones turn slower
// @param light float = 1 0..4 -- brightness of the rim light
// @param pulse float = 0.25 0..1 -- how much the light brightens on each beat
// @param warmth float = 0 0..1 -- 0 copper circles and one jade fret, 1 pale bronze circles and both frets jade
// @param thickness float = 0.01 0.001..0.1 -- metres across a segment; it is drawn as thick as the wireframe's line_width at any size

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TAU: f32 = 6.2831853;
const RINGS: u32 = 7u;
const SEGMENTS: u32 = 472u;
// The memory's texels after the rings.
const CLOCK: i32 = 7;
const STATE: i32 = 8;
// Presence where an arrival starts on the way up and a leaving on the way down.
const RISE_AT: f32 = 0.25;
const FALL_AT: f32 = 0.5;
// A move of presence between two frames: more than rounding, less than a cut.
const MOVE_MIN: f32 = 0.0005;
const MOVE_MAX: f32 = 0.1;
// Bars between two frames that count as a jump.
const JUMP_BARS: f32 = 2.0;
// An arrival has one step per ring and one to settle.
const STEPS: f32 = 8.0;
// A ring's fold takes this share of the leaving; the next ring inwards starts when the fold is
// STAGGER along. 0.4 * (1 + 6 * 0.25) = 1, so seven rings fill the leaving.
const FOLD_SHARE: f32 = 0.4;
const STAGGER: f32 = 0.25;
// Steps a head takes to fade once its ring has closed.
const HEAD_FADE: f32 = 0.6;
// How sharply the bright line behind a head falls off, per turn of the ring.
const TAIL: f32 = 7.0;
// The spark on a head: two short crossed lines, half length in metres at full light.
// Steps the light after the last ring's closing takes to fade.
const GLOW_STEPS: f32 = 2.0;
const SPARK_LENGTH: f32 = 0.14;
const SPARKS: u32 = 14u;

// A ring: what it is (0 circle, 1 step fret, 2 ticks), its inner and outer radius in metres at
// size 1, its cells (a fret's cells, a circle's or the ticks' segments), the first texel it
// takes and its texels.
struct Ring {
    kind: u32,
    r0: f32,
    r1: f32,
    cells: u32,
    first: u32,
    count: u32,
}

fn ring(k: u32) -> Ring {
    switch k {
        case 0u: { return Ring(0u, 0.30, 0.30, 32u, 0u, 32u); }
        case 1u: { return Ring(1u, 0.44, 0.62, 8u, 32u, 96u); }
        case 2u: { return Ring(0u, 0.78, 0.78, 48u, 128u, 48u); }
        case 3u: { return Ring(2u, 0.90, 0.97, 24u, 176u, 24u); }
        case 4u: { return Ring(1u, 1.08, 1.34, 12u, 200u, 144u); }
        case 5u: { return Ring(0u, 1.50, 1.50, 64u, 344u, 64u); }
        default: { return Ring(0u, 1.58, 1.58, 64u, 408u, 64u); }
    }
}

// The step fret's 12 segments in one cell: (along the cell, out from the inner radius) from and
// to, both 0..1. Three stairs up, the run along the top, the drop, the run to the next cell, and
// a hook hanging from the drop.
fn fret(j: u32) -> vec4f {
    var f = array<vec4f, 12>(
        vec4f(0.00, 0.00, 0.12, 0.00), vec4f(0.12, 0.00, 0.12, 0.33), vec4f(0.12, 0.33, 0.24, 0.33),
        vec4f(0.24, 0.33, 0.24, 0.66), vec4f(0.24, 0.66, 0.36, 0.66), vec4f(0.36, 0.66, 0.36, 1.00),
        vec4f(0.36, 1.00, 0.82, 1.00), vec4f(0.82, 1.00, 0.82, 0.00), vec4f(0.82, 0.00, 1.00, 0.00),
        vec4f(0.82, 0.28, 0.54, 0.28), vec4f(0.54, 0.28, 0.54, 0.70), vec4f(0.54, 0.70, 0.68, 0.70));
    return f[min(j, 11u)];
}

// Turns p about the unit axis u by angle a (Rodrigues).
fn turned(p: vec3f, u: vec3f, a: f32) -> vec3f {
    return p * cos(a) + cross(u, p) * sin(a) + u * dot(u, p) * (1.0 - cos(a));
}

// A ring's state from the memory: how far drawn, how far folded, its head's light. Without a memory
// the rings follow presence at once.
fn ring_state(k: u32) -> vec3f {
    if textureDimensions(in1).x < 2u {
        return vec3f(select(0.0, 1.0, params.presence >= RISE_AT), 0.0, 0.0);
    }
    return textureLoad(in1, vec2i(i32(k), 0), 0).rgb;
}

fn glow_after() -> f32 {
    if textureDimensions(in1).x < 2u {
        return 0.0;
    }
    return textureLoad(in1, vec2i(STATE, 0), 0).a;
}

fn lane_x(y: f32) -> f32 {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    return select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, lane0.b > 0.0);
}

struct Segment {
    a: vec3f,
    b: vec3f,
    ring: u32,
    shown: bool,
    drawn: f32,
    folded: f32,
    head: f32,
    along: f32,
    spark: f32,
}

fn segment(i: u32, opening: f32) -> Segment {
    var s: Segment;
    s.shown = false;
    s.a = vec3f(0.0);
    s.b = vec3f(0.0);
    s.ring = 0u;
    s.drawn = 0.0;
    s.folded = 0.0;
    s.head = 0.0;
    s.along = 0.0;
    s.spark = 0.0;
    if i >= SEGMENTS {
        return s;
    }
    var k = 0u;
    for (var n = 1u; n < RINGS; n++) {
        if i >= ring(n).first {
            k = n;
        }
    }
    let r = ring(k);
    let j = i - r.first;
    s.ring = k;
    // In the ring's own plane: (angle in turns, radius in metres) at both ends.
    var ends: vec4f;
    if r.kind == 1u {
        let per = r.count / r.cells;
        let cell = f32(j / per);
        let f = fret(j % per);
        ends = vec4f((cell + f.x) / f32(r.cells), mix(r.r0, r.r1, f.y), (cell + f.z) / f32(r.cells), mix(r.r0, r.r1, f.w));
    } else if r.kind == 2u {
        let u = (f32(j) + 0.5) / f32(r.cells);
        ends = vec4f(u, r.r0, u, r.r1);
    } else {
        ends = vec4f(f32(j) / f32(r.cells), r.r0, f32(j + 1u) / f32(r.cells), r.r0);
    }
    let state = ring_state(k);
    s.drawn = state.x;
    s.head = state.z;
    s.along = (f32(j) + 0.5) / f32(r.count);
    // The arrival: the line is drawn up to the head, the segment at the head only partly.
    let part = clamp(state.x * f32(r.count) - f32(j), 0.0, 1.0);
    if part <= 0.0 {
        return s;
    }
    s.shown = true;
    let folded = state.y * state.y * (3.0 - 2.0 * state.y);
    s.folded = folded;

    let fk = f32(k);
    let sign = select(-1.0, 1.0, k % 2u == 0u);
    // Each ring turns in its plane, the outer ones slower.
    let spin = sign * params.beats / (max(params.turn, 1.0) * (1.0 + 0.35 * fk));
    // Opening: larger, apart along the view, tilted about an axis that wanders slowly.
    let grow = max(params.size, 0.01) * (1.0 + opening * params.spread * (0.4 + fk / 6.0));
    let axis_angle = 0.9 * fk + params.beats / (4.0 * max(params.turn, 1.0));
    let axis = vec3f(cos(axis_angle), 0.0, sin(axis_angle));
    let lean = radians(params.tilt) * opening * sign * (0.6 + 0.4 * fk / 6.0) * (1.0 - folded);
    let ahead = params.depth * opening * (fk - 3.0) * (1.0 - folded);

    let lane = lane_x(params.at);
    let bob = 0.06 * sin(TAU * params.beats / 32.0);
    // Open, the rings rise a little, so the grown outer ones stay out of the water.
    let centre = vec3f(lane * params.follow, params.at + ahead, params.hover + bob + 0.9 * params.spread * opening);

    let ta = TAU * (ends.x + spin);
    let tb = TAU * (ends.z + spin);
    let pa = vec3f(ends.y * cos(ta), 0.0, ends.y * sin(ta)) * grow;
    let pb = vec3f(ends.w * cos(tb), 0.0, ends.w * sin(tb)) * grow;
    // Folding: the ring turns about the line across the view until it lies flat, edge on.
    let across = vec3f(1.0, 0.0, 0.0);
    let flat_ = 1.5707963 * folded * sign;
    let fa = turned(turned(pa, axis, lean), across, flat_);
    let fb = turned(turned(pb, axis, lean), across, flat_);
    s.a = centre + fa;
    s.b = centre + mix(fa, fb, part);
    return s;
}

// One texel of the memory. in1 is the memory of the last frame.
fn memory(x: i32) -> vec4f {
    let clock = textureLoad(in1, vec2i(CLOCK, 0), 0);
    let state = textureLoad(in1, vec2i(STATE, 0), 0);
    let whole = floor(max(params.bar_index, 0.0));
    let fraction = clamp(params.bar, 0.0, 1.0);
    let wrapped = whole - 1024.0 * floor(whole / 1024.0);
    var dt = wrapped - clock.r;
    if dt < -512.0 {
        dt += 1024.0;
    }
    dt += fraction - clock.g;
    let jump = clock.a < 0.5 || dt < -0.01 || dt > JUMP_BARS;
    dt = max(dt, 0.0);
    let step_bars = max(params.arrive_bars, 0.5) / STEPS;
    let at_step = (whole + fraction) / step_bars;
    let step_now = floor(at_step);
    let step_wrapped = step_now - 1024.0 * floor(step_now / 1024.0);
    let phase = at_step - step_now;
    let new_step = step_wrapped != clock.b;

    let p = clamp(params.presence, 0.0, 1.0);
    let moved = p - state.r;
    let rising = moved > MOVE_MIN && moved < MOVE_MAX;
    let falling = moved < -MOVE_MIN;
    // Between the two marks presence going up or down decides; standing still it keeps the state.
    var wanted = p >= FALL_AT;
    if !wanted && p >= RISE_AT {
        wanted = rising || (!falling && state.b > 1.5);
    }

    if x == CLOCK {
        return vec4f(wrapped, fraction, step_wrapped, 1.0);
    }
    let settling = state.b > 0.5 && state.b < 1.5;

    // The ring being drawn: a new step closes the last one and takes the innermost ring not whole.
    let was = i32(round(state.g)) - 1;
    var drawing = was;
    if !wanted {
        drawing = -1;
    } else if new_step {
        drawing = -1;
        for (var n = 6; n >= 0; n--) {
            if n != was && textureLoad(in1, vec2i(n, 0), 0).r < 1.0 {
                drawing = n;
            }
        }
    }

    if x == STATE {
        var glow = max(state.a - dt / (GLOW_STEPS * step_bars), 0.0);
        if jump || settling {
            glow = 0.0;
        } else if wanted && new_step && was == 6 {
            glow = 1.0;
        }
        return vec4f(p, f32(drawing + 1), select(select(0.0, 2.0, wanted), 1.0, jump), glow);
    }

    // After a jump: nothing for one frame, then whole unless an arrival is yet to come.
    if jump {
        return vec4f(0.0);
    }
    if settling {
        let whole_now = p >= RISE_AT && !rising;
        return vec4f(select(0.0, 1.0, whole_now), 0.0, 0.0, 0.0);
    }

    let me = textureLoad(in1, vec2i(x, 0), 0);
    var drawn = me.r;
    var fold = me.g;
    var head = me.b;
    let fold_bars = max(FOLD_SHARE * params.leave_bars, 0.01);
    if wanted {
        fold = max(fold - dt / fold_bars, 0.0);
        if new_step && x == was {
            drawn = 1.0;
        }
        if x == drawing {
            if phase > drawn {
                drawn = phase;
                head = 1.0;
            }
        } else {
            head = max(head - dt / (HEAD_FADE * step_bars), 0.0);
        }
        return vec4f(drawn, fold, head, 0.0);
    }
    head = max(head - dt / (HEAD_FADE * step_bars), 0.0);
    // Leaving: a ring folds once every ring outside it that is there has folded far enough.
    var free = drawn > 0.0;
    for (var n = x + 1; n < 7; n++) {
        let o = textureLoad(in1, vec2i(n, 0), 0);
        if o.r > 0.0 && o.g < STAGGER {
            free = false;
        }
    }
    if free {
        fold = fold + dt / fold_bars;
    }
    if fold >= 1.0 {
        return vec4f(0.0);
    }
    return vec4f(drawn, fold, head, 0.0);
}

// The spark on ring k's head: two crossed lines through the end of the line drawn so far.
fn spark(i: u32, opening: f32) -> Segment {
    var s = segment(SEGMENTS, opening);
    let n = i - SEGMENTS;
    let k = n / 2u;
    let state = ring_state(k);
    if state.z <= 0.01 || state.x <= 0.0 || state.y > 0.0 {
        return s;
    }
    let r = ring(k);
    var end = segment(r.first + min(u32(state.x * f32(r.count)), r.count - 1u), opening);
    if !end.shown {
        end = segment(r.first + u32(state.x * f32(r.count)) - 1u, opening);
    }
    if !end.shown {
        return s;
    }
    let tip = end.b;
    let turn = params.beats * 0.5 + f32(n / 2u) + 0.25 * f32(n % 2u) * TAU;
    let arm = (SPARK_LENGTH * state.z * state.z) * vec3f(cos(turn), 0.0, sin(turn));
    s.a = tip - arm;
    s.b = tip + arm;
    s.shown = true;
    s.spark = state.z;
    return s;
}

fn any_segment(i: u32, opening: f32) -> Segment {
    if i >= SEGMENTS && i < SEGMENTS + SPARKS {
        return spark(i, opening);
    }
    return segment(i, opening);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if params.what == what_memory {
        return memory(i32(cell.x));
    }
    let i = cell.y * 32u + cell.x;
    let swell = clamp(params.swell, 0.0, 1.0);
    let snap = clamp(params.snap, 0.0, 1.0);
    let lo = min(params.open_at, params.open_full - 0.01);
    let opening = max(smoothstep(lo, params.open_full, swell) * (1.0 - snap), clamp(params.open, 0.0, 1.0));
    let s = any_segment(i, opening);
    let d = s.b - s.a;
    var len = length(d);
    if !s.shown {
        len = 0.0;
    }
    if params.what == what_aim {
        return vec4f(select(vec3f(0.0, 1.0, 0.0), d / max(len, 1e-6), len > 1e-6), 1.0);
    }
    if params.what == what_scale {
        let t = select(0.0, params.thickness, len > 0.0);
        // A little longer than the segment, so the corners close.
        return vec4f(t, len + select(0.0, 0.5 * params.thickness, len > 0.0), t, 1.0);
    }
    if params.what == what_color {
        if s.spark > 0.0 {
            return vec4f(vec3f(1.0, 0.62, 0.28) * 1.8 * s.spark * s.spark, 1.0);
        }
        let m = 0.5 * (s.a + s.b);
        // The band lies where the line from the eye to the horizon passes the segment.
        let band = max(params.eye, 0.0) * (1.0 - max(m.y, 0.0) / max(params.reach, 1.0));
        let rim = 0.3 + 0.7 * exp(-pow((m.z - band) / 0.8, 2.0));
        let b = clamp(params.beat, 0.0, 1.0);
        let kick = smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
        let dawn = clamp(params.dawn, 0.0, 1.0);
        let glow = glow_after();
        let fade = (1.0 - s.folded) * (1.0 - s.folded);
        let level = params.light * rim * (0.3 + 0.25 * dawn + 0.45 * opening + 0.35 * snap + params.pulse * kick + 0.5 * glow);
        let warm = clamp(params.warmth, 0.0, 1.0);
        let copper = mix(vec3f(0.55, 0.19, 0.055), vec3f(0.30, 0.22, 0.15), warm);
        let jade = vec3f(0.10, 0.40, 0.28);
        var tone = copper;
        if s.ring == 1u {
            tone = jade;
        } else if s.ring == 4u {
            tone = mix(copper, jade, warm);
        }
        // The head: a hot point and a line behind it that cools towards the ring's own light.
        let behind = max(s.drawn - s.along, 0.0);
        let tail = s.head * exp(-TAIL * behind);
        let ember = vec3f(1.0, 0.42, 0.10) * 1.3 * tail + vec3f(1.0, 0.75, 0.45) * 3.0 * pow(tail, 12.0);
        // Alpha fades a folding ring: a dark line would stand out against the sky.
        return vec4f(tone * level + ember, fade);
    }
    return vec4f(s.a + 0.5 * d, 1.0);
}
