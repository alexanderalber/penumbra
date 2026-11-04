// The beams of mid_traps, one per texel, for geo.tex_instance with a thin prism one unit long
// along y. Laser beams run across a tube from one point on its wall to another. They stand in
// the world, a slice of beams every `spacing` beats of travel, and come towards the camera with
// it. Together they make a maze with one free corridor through it. The corridor's middle wanders
// from side to side and up and down along the tube, a knot every `corridor_bars` bars (see
// `corridor` below). No beam comes nearer to the corridor's middle than `open` metres, where the
// glider passes the beam. mid_glider's flight 4 follows the same corridor.
// A beam that would pass within `eye` metres of the camera's line (the stage's camera height,
// straight ahead) stays dark, so the camera never flies through one.
// Each beam tries up to 8 seeded lines and keeps the first one clear of the corridor; with none
// clear it is left out. Most lines pass the corridor closely, the rest join two points of the wall
// drawn anywhere. That choice depends on the beam's place in the world only, so a beam never jumps
// while it comes nearer. The beams stay put while the tube rolls: their ends stay on the round
// wall, and the corridor stays where the glider and the camera are.
// `presence` switches the beams on from the far end of the tube towards the camera as it rises,
// and off from the camera towards the far end as it falls. Each beam fades over a few metres of
// that front, so none pops.
// in0: the stage's lane picture (its `lane` output); only its `step` is read. Unconnected:
// `step` metres per beat.
// Column k of the picture is slice k, row j its beam j. Render it 64 x 4.
// `what` place: the middle of each beam in rgb. aim: its direction. scale: `thickness` across,
// its length along y (0 for none). light: its colour.
// @param what choice(place|aim|scale|light) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the beams flare on its start
// @param presence float = 1 0..1 -- 1 every beam, towards 0 they go dark from the camera towards the far end
// @param seed float = 1 0..1000 -- another number gives another corridor and other beams
// @param corridor_bars float = 2 0.5..8 -- bars of travel from one knot of the corridor to the next; the corridor stands at a knot on a downbeat
// @param wander float = 1.2 0..2.5 -- metres the corridor's middle moves to either side of the tube's middle
// @param climb float = 0.5 0..1.5 -- metres the corridor's middle moves up and down
// @param middle float = 2 0.5..4 -- metres above the tube's bottom where the corridor's middle lies on average
// @param open float = 0.9 0.4..2 -- radius of the free corridor, metres
// @param eye float = 0.6 0..2 -- radius of the free space round the camera's line, metres
// @param beams int = 2 1..4 -- beams per slice: 2 make a maze with room to see through it
// @param spacing float = 1 0.25..8 -- beats of travel from one slice of beams to the next
// @param slant float = 1 0..4 -- metres one end of a beam may lie ahead of the other
// @param brightness float = 1 0..4 -- the beams' light
// @param flicker float = 0.3 0..1 -- how much each beam flickers by itself
// @param color color = [1, 0.06, 0.1, 1] -- the beams' colour
// @param thickness float = 0.015 0.001..0.1 -- metres across a beam; the line is drawn as thick as the wireframe's line_width at any size
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step
// @param eye_height float = 1.8 0..10 -- the camera's height, metres (height of the stage's pose)
// @param far float = 20 1..100 -- metres ahead where the tube fades out (far of the stage's pose)
// @param axis float = 4 0..20 -- the tube's axis, metres over the floor (axis of the stage's pose)
// @param radius float = 4 0..20 -- the tube's radius, metres (radius of the stage's pose)
// @param curve float = 1 0..1 -- 1 where the stage is a tube (curve of the stage's pose); towards 0 the beams go dark

const SLICES: u32 = 64u;
const ROWS: u32 = 4u;
// Slices behind the camera.
const BEHIND: f32 = 3.0;
// Pairs of points a beam tries.
const TRIES: u32 = 8u;
// Metres before and behind a beam over which the corridor stays clear. The glider flies along the
// corridor, so where it crosses a beam's plane it is near the corridor's middle there.
const BODY: f32 = 0.5;
// Points along that stretch where the corridor is checked.
const SAMPLES: i32 = 7;
// Metres a beam's ends lie inside the wall, so the wall's rings stay in front of them.
const INSET: f32 = 0.03;
// The share of beams that pass the corridor closely, and how much further than `open` they
// pass it at most, metres.
const CLOSE: f32 = 0.75;
const NEAR: f32 = 1.2;
// Metres over which a beam fades as the presence front passes it.
const FRONT: f32 = 4.0;
const PI: f32 = 3.14159265;
const TAU: f32 = 6.2831853;

// corridor begin: library/mid_glider/steer.wgsl holds the same code from here to "corridor end".
// The free corridor of mid_traps: where its middle lies across (x, metres from the middle) and
// up (z, metres above the floor) at `bars` bars of travel, a whole number on a downbeat. The
// corridor has a knot every `every` bars: left of the middle at one knot, right at the next, and
// up or down by up to `climb`. It holds at a knot for an eighth of the way to the next one and
// then glides there. `seed` picks the knots.
fn corridor_knot(n: f32, seed: f32, wander: f32, climb: f32, middle: f32) -> vec2f {
    let k = i32(n);
    let h = pn_hash(vec3u(bitcast<u32>(k), bitcast<u32>(i32(seed)), 0x7a95u));
    let across = 0.35 + 0.65 * f32(h.x) / 4294967296.0;
    let up = 2.0 * f32(h.y) / 4294967296.0 - 1.0;
    let side = select(-1.0, 1.0, (k & 1) == 0);
    return vec2f(side * max(wander, 0.0) * across, middle + max(climb, 0.0) * up);
}

fn corridor(bars: f32, every: f32, seed: f32, wander: f32, climb: f32, middle: f32) -> vec2f {
    let t = bars / max(every, 0.5);
    let n = floor(t);
    let f = clamp((t - n - 0.125) / 0.875, 0.0, 1.0);
    let s = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = corridor_knot(n, seed, wander, climb, middle);
    let b = corridor_knot(n + 1.0, seed, wander, climb, middle);
    return mix(a, b, s);
}
// corridor end

fn unit(h: u32) -> f32 {
    return f32(h) / 4294967296.0;
}

// Metres per beat: the stage's, from its lane picture, else `step`.
fn step_now() -> f32 {
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    return max(select(params.step, lane0.b, lane0.b > 0.0), 0.01);
}

// The corridor's middle `y` metres ahead.
fn corridor_at(y: f32, step: f32) -> vec2f {
    let bpb = max(params.beats_per_bar, 1.0);
    let bars = (params.beats + y / step) / bpb + params.drift;
    return corridor(bars, params.corridor_bars, params.seed, params.wander, params.climb, params.middle);
}

// The distance from point p to the segment from a to b.
fn to_segment(a: vec2f, b: vec2f, p: vec2f) -> f32 {
    let ab = b - a;
    let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(a + ab * t - p);
}

// Whether a beam across the tube from a to b (x and z), between y0 and y1 metres ahead, keeps
// `open` metres from the corridor's middle over the glider's length around it.
fn clear(a: vec2f, b: vec2f, y0: f32, y1: f32, step: f32) -> bool {
    let lo = min(y0, y1) - BODY;
    let hi = max(y0, y1) + BODY;
    for (var i = 0; i < SAMPLES; i = i + 1) {
        let y = mix(lo, hi, f32(i) / f32(SAMPLES - 1));
        if to_segment(a, b, corridor_at(y, step)) < params.open {
            return false;
        }
    }
    return true;
}

// A point on the wall at angle `a` round the axis, 0 at the bottom.
fn on_wall(a: f32, r: f32) -> vec2f {
    return vec2f(r * sin(a), params.axis - r * cos(a));
}

// The ends of a beam drawn by `h`, on the wall of radius `r`: xy one end, zw the other; all
// 1e9 where the draw gives none. Most beams pass the corridor's middle `here` closely, at a
// little more than `open` on a side drawn by `h`, at any angle: they make the maze round the
// corridor. The others run between two points of the wall drawn anywhere.
fn beam_ends(h: vec3u, here: vec2f, r: f32) -> vec4f {
    if unit(pn_hash(h).x) < CLOSE {
        let turn = TAU * unit(h.x);
        let normal = vec2f(cos(turn), sin(turn));
        let along = vec2f(-normal.y, normal.x);
        let off = select(-1.0, 1.0, (h.z & 2u) == 0u) * (params.open + NEAR * unit(h.y));
        let foot = here + normal * off - vec2f(0.0, params.axis);
        // Where the line through `foot` along `along` meets the wall.
        let b = dot(along, foot);
        let disc = b * b - (dot(foot, foot) - r * r);
        if disc <= 0.0 {
            return vec4f(1e9);
        }
        let root = sqrt(disc);
        let axis = vec2f(0.0, params.axis);
        return vec4f(axis + foot + along * (-b - root), axis + foot + along * (-b + root));
    }
    let first = TAU * unit(h.x);
    let span = PI * (0.35 + 0.65 * unit(h.y));
    let second = first + select(-span, span, (h.z & 1u) == 0u);
    return vec4f(on_wall(first, r), on_wall(second, r));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let j = cell.y;
    let step = step_now();
    let spacing = max(params.spacing, 0.25);
    // Slice k stands on beat n * spacing of the world, so it comes nearer with the world.
    let n = floor(params.beats / spacing) - BEHIND + f32(cell.x);
    let y = (n * spacing - params.beats) * step;
    let r = max(params.radius - INSET, 0.0);
    let far = max(params.far, 1.0);
    let wanted = cell.x < SLICES && j < ROWS && i32(j) < params.beams && y <= far + 2.0 && r > 0.5;

    // The first of the seeded pairs of points that keeps clear of the corridor.
    var a = vec2f(0.0);
    var b = vec2f(0.0);
    var ahead = 0.0;
    var found = false;
    var draw = 0.0;
    if wanted {
        let here = corridor_at(y, step);
        for (var t = 0u; t < TRIES; t = t + 1u) {
            let h = pn_hash(vec3u(bitcast<u32>(i32(n)), j * 16u + t, bitcast<u32>(i32(params.seed)) + 0x3c1u));
            let ends = beam_ends(h, here, r);
            let lean = params.slant * (2.0 * unit(h.z) - 1.0);
            if ends.x < 1e8 && clear(ends.xy, ends.zw, y, y + lean, step) {
                a = ends.xy;
                b = ends.zw;
                ahead = lean;
                found = true;
                draw = unit(h.y ^ h.z);
                break;
            }
        }
    }
    let start = vec3f(a.x, y, a.y);
    let end = vec3f(b.x, y + ahead, b.y);
    let mid = 0.5 * (start + end);

    // The presence front: on from the far end inwards as presence rises, off from the camera
    // outwards as it falls.
    let p = clamp(params.presence, 0.0, 1.0) * clamp(params.curve, 0.0, 1.0);
    let toward = far - mid.y;
    let on = smoothstep(0.0, 1.0, (p * (far + 2.0 * FRONT) - toward - draw * FRONT) / FRONT);
    // Dark near the camera's line.
    let eye = max(params.eye, 0.0);
    let gap = to_segment(a, b, vec2f(0.0, params.eye_height));
    let seen = select(1.0, smoothstep(0.5 * eye, eye, gap), eye > 0.0);
    let lit = select(0.0, on * seen, found && mid.y > -2.0);

    let w = params.what;
    if w == what_place {
        return vec4f(mid, 1.0);
    }
    if w == what_aim {
        let d = end - start;
        return vec4f(select(vec3f(0.0, 1.0, 0.0), d, length(d) > 1e-4), 1.0);
    }
    if w == what_scale {
        let shown = lit > 0.01;
        let t = select(0.0, params.thickness, shown);
        return vec4f(t, select(0.0, length(end - start), shown), t, 1.0);
    }
    // Light: each beam flickers by itself and flares on the beat.
    let id = n * 4.0 + f32(j);
    let flick = 1.0 - clamp(params.flicker, 0.0, 1.0) * 0.5 * (1.0 + pn_noise(vec3f(id * 0.37, params.beats * 2.0, 0.0), 13u));
    let fall = 1.0 - clamp(params.beat, 0.0, 1.0);
    let flare = 1.0 + 0.4 * fall * fall * fall * fall;
    let c = params.color.rgb * max(params.brightness, 0.0) * flick * flare * lit;
    return vec4f(c, 1.0);
}
