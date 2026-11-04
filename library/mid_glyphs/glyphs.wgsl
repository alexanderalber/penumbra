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
// @param what choice(place|aim|scale|color) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid): the rings turn with it
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the light pulses on its start
// @param swell float = 0 0..1 -- the stage's swell: opens the rings
// @param snap float = 0 0..1 -- the drop, held for a few seconds: closes the rings and lights them a little
// @param dawn float = 0 0..1 -- the stage's dawn: more light towards 1
// @param eye float = 1.2 0..10 -- the camera's height over the water, metres (height of the stage's pose)
// @param reach float = 40 1..200 -- metres ahead where the horizon lies (far of the stage's pose)
// @param presence float = 1 0..1 -- 1 all rings, towards 0 they draw back from the outside in, 0 none
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

struct Segment {
    a: vec3f,
    b: vec3f,
    ring: u32,
    shown: bool,
}

fn segment(i: u32, opening: f32) -> Segment {
    var s: Segment;
    s.shown = false;
    s.a = vec3f(0.0);
    s.b = vec3f(0.0);
    s.ring = 0u;
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
    // Presence: the rings draw back from the outside in, each one around its turn.
    let share = clamp(params.presence * 2.0 * f32(RINGS) - f32(k), 0.0, 1.0);
    if f32(j) + 0.5 > share * f32(r.count) {
        return s;
    }
    s.shown = true;

    let fk = f32(k);
    let sign = select(-1.0, 1.0, k % 2u == 0u);
    // Each ring turns in its plane, the outer ones slower.
    let spin = sign * params.beats / (max(params.turn, 1.0) * (1.0 + 0.35 * fk));
    // Opening: larger, apart along the view, tilted about an axis that wanders slowly.
    let grow = max(params.size, 0.01) * (1.0 + opening * params.spread * (0.4 + fk / 6.0));
    let axis_angle = 0.9 * fk + params.beats / (4.0 * max(params.turn, 1.0));
    let axis = vec3f(cos(axis_angle), 0.0, sin(axis_angle));
    let lean = radians(params.tilt) * opening * sign * (0.6 + 0.4 * fk / 6.0);
    let ahead = params.depth * opening * (fk - 3.0);

    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let u = (params.at - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, lane0.b > 0.0);
    let bob = 0.06 * sin(TAU * params.beats / 32.0);
    // Open, the rings rise a little, so the grown outer ones stay out of the water.
    let centre = vec3f(lane * params.follow, params.at + ahead, params.hover + bob + 0.9 * params.spread * opening);

    let ta = TAU * (ends.x + spin);
    let tb = TAU * (ends.z + spin);
    let pa = vec3f(ends.y * cos(ta), 0.0, ends.y * sin(ta)) * grow;
    let pb = vec3f(ends.w * cos(tb), 0.0, ends.w * sin(tb)) * grow;
    s.a = centre + turned(pa, axis, lean);
    s.b = centre + turned(pb, axis, lean);
    return s;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let i = cell.y * 32u + cell.x;
    let swell = clamp(params.swell, 0.0, 1.0);
    let snap = clamp(params.snap, 0.0, 1.0);
    let lo = min(params.open_at, params.open_full - 0.01);
    let opening = max(smoothstep(lo, params.open_full, swell) * (1.0 - snap), clamp(params.open, 0.0, 1.0));
    let s = segment(i, opening);
    let d = s.b - s.a;
    var len = length(d);
    if !s.shown || params.presence < 0.001 {
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
        let m = 0.5 * (s.a + s.b);
        // The band lies where the line from the eye to the horizon passes the segment.
        let band = max(params.eye, 0.0) * (1.0 - max(m.y, 0.0) / max(params.reach, 1.0));
        let rim = 0.3 + 0.7 * exp(-pow((m.z - band) / 0.8, 2.0));
        let b = clamp(params.beat, 0.0, 1.0);
        let kick = smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
        let dawn = clamp(params.dawn, 0.0, 1.0);
        let level = params.light * rim * (0.3 + 0.25 * dawn + 0.45 * opening + 0.35 * snap + params.pulse * kick);
        let warm = clamp(params.warmth, 0.0, 1.0);
        let copper = mix(vec3f(0.55, 0.19, 0.055), vec3f(0.30, 0.22, 0.15), warm);
        let jade = vec3f(0.10, 0.40, 0.28);
        var tone = copper;
        if s.ring == 1u {
            tone = jade;
        } else if s.ring == 4u {
            tone = mix(copper, jade, warm);
        }
        return vec4f(tone * level, 1.0);
    }
    return vec4f(s.a + 0.5 * d, 1.0);
}
