// The cells of mid_equalisers, one per texel, for geo.tex_instance with a unit box. Rings of
// equaliser bars stand on the wall of a tube, `every` bars of travel apart, and come towards the
// camera with the world, so the camera flies through them. Each bar stands on the wall and points
// towards the tube's axis. Round a ring the bars show the music's levels as sky_bars does: the
// bass in the middle of the row, the mids beside it, the treble at both ends, with an opening of
// the ring between them (`arc`). The bars jump up on each beat and fall over it. Each ring is turned `turn` degrees round the axis against the one before.
// The bars stay out of a round space of `open` metres round the flight, `middle` metres above
// the tube's bottom, so the glider and the camera pass untouched: there they are shorter.
// in0: the stage's lane picture (its `lane` output); only its `step` is read. Unconnected:
// `step` metres per beat.
// Column i of the picture is bar i of a ring; row r * 8 + c is cell c of ring r, counted from the
// wall. Render it 48 x 96.
// `what` place: the middle of each cell in rgb. aim: its direction (see `aim_of`). scale: its
// size along the box's axes (0 for none). light: its colour.
// @param what choice(place|aim|scale|light) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param drift float = 0 -100..100 -- where the bars stand against `beats`, in bars (drift of a chan.beat_grid)
// @param low float = 0 0..1 -- the bass (energy_low of a chan.beat_grid)
// @param mid float = 0 0..1 -- the mids (energy_mid)
// @param high float = 0 0..1 -- the treble (energy_high)
// @param onset float = 0 0..1 -- 1 in the frame of an onset (onset of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the bars jump up on its start and fall over it
// @param presence float = 1 0..1 -- 1 the bars stand at their level, towards 0 they shrink into the wall, 0 none
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the cells' light
// @param every float = 2 0.5..8 -- bars of travel from one ring to the next; a ring passes the camera on a downbeat
// @param turn float = 20 -180..180 -- degrees each ring is turned round the axis against the one before
// @param count int = 32 8..48 -- bars round a ring
// @param arc float = 270 90..360 -- degrees of the ring the row of bars covers; the rest is left open, so the turn from ring to ring shows
// @param cells int = 6 1..8 -- cells a full bar is stacked of; 1 draws solid bars
// @param tall float = 1.6 0.2..4 -- metres a bar stands out of the wall at full level
// @param open float = 2 0.5..4 -- radius of the free space round the flight, metres: no bar reaches into it
// @param middle float = 2 0..8 -- the free space's middle, metres above the tube's bottom
// @param gap float = 0.4 0..0.9 -- share of a bar's place left dark between two bars
// @param depth float = 0.05 0.01..1 -- metres a cell is deep, along the tube
// @param jitter float = 0.5 0..1 -- how much each bar moves by itself
// @param kick float = 0.25 0..1 -- how much an onset lifts the bars
// @param pump float = 0.4 0..1 -- how far the bars fall over each beat: 0 they follow the levels alone
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step
// @param near float = -2 -8..0 -- metres ahead where a ring leaves, behind the camera
// @param far float = 20 1..100 -- metres ahead where the tube fades out; rings stand up to one spacing beyond (far of the stage's pose)
// @param axis float = 4 0..20 -- the tube's axis, metres over the floor (axis of the stage's pose)
// @param radius float = 4 0..20 -- the tube's radius, metres (radius of the stage's pose)
// @param curve float = 1 0..1 -- 1 where the stage is a tube (curve of the stage's pose); towards 0 the bars shrink into the wall

const RINGS: u32 = 12u;
const CELLS: u32 = 8u;
const BARS: u32 = 48u;
const TAU: f32 = 6.2831853;
const PI: f32 = 3.14159265;
// Metres a bar's foot stands inside the wall, so the wall's lines stay in front of it.
const INSET: f32 = 0.08;
// The share of a cell's length that is lit; the rest is the dark line between two cells.
const FILL: f32 = 0.62;
// Metres between a bar's tip and the free space.
const MARGIN: f32 = 0.15;

fn hash(a: u32, b: u32, c: u32) -> f32 {
    return f32(pn_hash(vec3u(a, b, c)).x) / 4294967296.0;
}

// The palette of sky_bars: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

// Bar `i`'s level of `n` in ring `ring`, 0 to 1: the bands blended across the row, each bar
// moving a little by itself, an onset lifting every bar.
fn level(i: u32, n: u32, ring: i32) -> f32 {
    let b = abs((f32(i) + 0.5) / f32(n) * 2.0 - 1.0);
    let band = mix(mix(params.low, params.mid, smoothstep(0.1, 0.55, b)), params.high, smoothstep(0.55, 0.95, b));
    let wobble = pn_noise(vec3f(f32(i) * 1.37, params.beats * 1.3, f32(ring) * 3.1), 21u);
    let own = clamp(1.0 + params.jitter * wobble, 0.0, 1.6);
    let fall = 1.0 - clamp(params.beat, 0.0, 1.0);
    let pumped = 1.0 - clamp(params.pump, 0.0, 1.0) * (1.0 - fall * fall);
    let lift = params.kick * clamp(params.onset, 0.0, 1.0) * (0.6 + 0.4 * hash(i, bitcast<u32>(ring), 7u));
    return clamp(clamp(band, 0.0, 1.0) * own * pumped + lift, 0.0, 1.0);
}

// How far a bar may stand out of the wall from its foot `foot` towards the axis (`inward`, a
// unit vector across the tube) before it reaches the free space round `centre` (all in the ring's
// plane: x across, y up).
fn room(foot: vec2f, inward: vec2f, centre: vec2f) -> f32 {
    let open = max(params.open, 0.0);
    let to = foot - centre;
    let c = dot(to, to) - open * open;
    if c <= 0.0 {
        return 0.0;
    }
    let b = dot(inward, to);
    let disc = b * b - c;
    if disc < 0.0 || b >= 0.0 {
        return 1e6;
    }
    return max(-b - sqrt(disc) - MARGIN, 0.0);
}

// The box's +y is turned to this direction with its +z kept up, so a direction in the ring's
// plane leaves the box's x along the tube. Along the ring (the tangent) where that lies mostly
// across, else along the bar: `.w` 1 where the box's y runs along the bar.
fn aim_of(outward: vec2f) -> vec4f {
    let tangent = vec2f(outward.y, -outward.x);
    if abs(tangent.x) >= abs(outward.x) {
        return vec4f(tangent.x, 0.0, tangent.y, 0.0);
    }
    return vec4f(outward.x, 0.0, outward.y, 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let bar = cell.x;
    let ring = cell.y / CELLS;
    let c = cell.y % CELLS;
    let n = u32(clamp(params.count, 8, i32(BARS)));
    let cells = u32(clamp(params.cells, 1, i32(CELLS)));
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let step = select(params.step, lane0.b, lane0.b > 0.0);
    // Ring k stands at y = spacing * (k - part), so a ring passes y = 0 where the bar count is a
    // multiple of `every`. Only the travel's fraction moves the rings.
    let bpb = max(params.beats_per_bar, 1.0);
    let every = max(params.every, 0.25);
    let spacing = every * bpb * max(step, 0.01);
    let travel = (params.beats / bpb + params.drift) / every;
    let whole = floor(travel);
    let part = travel - whole;
    let k = ceil(params.near / spacing + part) + f32(ring);
    let y = spacing * (k - part);
    // The ring's own number, the same while it comes nearer.
    let number = i32(whole + k);

    // The ring's plane: x across, up from the floor. The bars' row turns with the ring's number.
    let r = max(params.radius, 0.0);
    let turned = radians(params.turn) * f32(number);
    let arc = radians(clamp(params.arc, 90.0, 360.0));
    let angle = turned + arc * ((f32(bar) + 0.5) / f32(n) - 0.5);
    let outward = vec2f(sin(angle), cos(angle));
    let axis = vec2f(0.0, params.axis);
    let foot = axis + outward * max(r - INSET, 0.0);
    let centre = vec2f(0.0, params.axis - r + params.middle);
    let longest = min(min(max(params.tall, 0.0), room(foot, -outward, centre)), r);
    let p = clamp(params.presence, 0.0, 1.0) * clamp(params.curve, 0.0, 1.0);
    let shown = p * p * (3.0 - 2.0 * p);
    let len = level(bar, n, number) * longest * shown;
    // Cell c covers its share of the longest bar; the bar's tip lies in the last cell it reaches.
    let unit = longest / f32(cells);
    let base = f32(c) * unit;
    let lit = clamp(len - base, 0.0, unit * FILL);
    let on = bar < n && c < cells && y <= max(params.far, 1.0) + spacing && lit > 0.005 && r > 0.1;
    let out = base + 0.5 * lit;
    let at = foot - outward * out;
    let w = params.what;
    if w == what_place {
        return vec4f(at.x, y, at.y, 1.0);
    }
    if w == what_aim {
        let a = aim_of(outward);
        return vec4f(a.xyz, 1.0);
    }
    if w == what_scale {
        if !on {
            return vec4f(0.0, 0.0, 0.0, 1.0);
        }
        // The cell is as wide as its share of the ring where it stands, less the gap.
        let across = (1.0 - clamp(params.gap, 0.0, 0.9)) * arc * max(r - INSET - out, 0.05) / f32(n);
        let along = aim_of(outward).w > 0.5;
        let d = max(params.depth, 0.001);
        return vec4f(d, select(across, lit, along), select(lit, across, along), 1.0);
    }
    // Light: the palette from the wall (magenta, violet) to the tip (yellow, cyan).
    let up = (f32(c) + 0.5) / f32(cells);
    let colour = palette(1.0 - up) * 0.9 * max(params.brightness, 0.0);
    return vec4f(select(vec3f(0.0), colour, on), 1.0);
}
