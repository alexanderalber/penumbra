// The rings of mid_rings, one per texel, for geo.tex_instance with ring.obj and ring_fill.obj.
// Broad rings stand around stage_ascent's tower, outside its wall, and pass downwards with the
// storeys as the camera climbs. Not every storey has one: a ring sits halfway between two
// storeys where the storey's draw lies under `chance` and under its neighbours', so never on
// two storeys in a row. Each
// ring is tilted by up to `tilt` degrees and turns once in `orbit` beats, half of them one way and
// half the other, so they turn against each other and against the tower.
// Towards `presence` 0, or where the stage lies down as a tube (the pose's `curve` towards 1),
// the rings widen out into the distance and dim, so they fade into the sky.
// Texel k is the ring of the storey k - 3 above the camera's, as stage_ascent counts its
// storeys. Render it 12 x 1.
// `what` place: the ring's middle, on the tower's axis. aim: the direction its +y turns to,
// which tilts and turns it. scale: its radius in x and y, its height in z (0 for none). light:
// the colour of its edges.
// @param what choice(place|aim|scale|light) = place -- what this node writes
// @param whole float = 0 0..20000 -- the whole beats travelled (travelled of a chan.beat_grid less its fraction)
// @param phase float = 0 0..1 -- the fraction of the beat travelled
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the edges brighten with it
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the edges flash on its start
// @param presence float = 1 0..1 -- 1 the rings stand round the tower, towards 0 they widen out into the distance and dim
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the edges' light
// @param storey float = 3 1..10 -- metres from one storey to the next (the stage's `ceiling`)
// @param storey_beats float = 4 1..16 -- beats the camera takes to climb one storey (the stage's `storey_beats`)
// @param chance float = 0.35 0..1 -- how many storeys have a ring: at 1 about every third, never two in a row
// @param inner float = 7.5 1..40 -- metres from the axis to the smallest ring's outer edge
// @param outer float = 10 1..40 -- metres from the axis to the largest ring's outer edge
// @param band float = 0.35 0.02..3 -- metres a ring's band is high
// @param tilt float = 10 0..45 -- degrees the most tilted ring leans
// @param orbit float = 256 8..4096 -- beats a ring takes to turn once
// @param pulse float = 0.5 0..2 -- how much the edges flash on each beat
// @param radius float = 6 0..20 -- the tower's radius, metres (radius of the stage's pose): its axis stands half of it ahead of the camera
// @param curve float = 0 0..1 -- 0 where the tower stands (curve of the stage's pose); towards 1 the rings fade out as by `presence`

const RINGS: u32 = 12u;
// Storeys below the camera's: the lowest ring drawn belongs to the storey this many down.
const BELOW: f32 = 3.0;
const TAU: f32 = 6.2831853;
const DEG: f32 = 0.017453293;

fn hash(n: f32, salt: u32) -> f32 {
    return f32(pn_hash(vec3u(bitcast<u32>(i32(n)), salt, 11u)).x) / 4294967296.0;
}

// Storeys climbed: the whole ones and the fraction of the one under way.
fn climbed() -> vec2f {
    let sb = max(params.storey_beats, 0.25);
    // `whole` is a whole number, so its remainder is exact in f32.
    let rest = params.whole - sb * floor(params.whole / sb) + params.phase;
    let past = floor(params.whole / sb) + floor(rest / sb);
    let f = rest / sb - floor(rest / sb);
    return vec2f(past, f);
}

// Whether storey n has a ring: its draw under `chance` and below the draws of the storeys on
// either side, so two storeys in a row never both have one.
fn has_ring(n: f32) -> bool {
    let d = hash(n, 3u);
    return d < clamp(params.chance, 0.0, 1.0) && d <= hash(n - 1.0, 3u) && d < hash(n + 1.0, 3u);
}

// How much of the rings is there: `presence`, less where the tower lies down.
fn there() -> f32 {
    return clamp(params.presence, 0.0, 1.0) * (1.0 - clamp(params.curve, 0.0, 1.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    if cell.x >= RINGS || cell.y >= 1u {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let k = f32(cell.x);
    let c = climbed();
    let n = c.x + k - BELOW;
    let q = there();
    let w = params.what;

    if w == what_place {
        let z = (k - BELOW - c.y + 0.5) * params.storey;
        return vec4f(0.0, 0.5 * max(params.radius, 0.0), z, 1.0);
    }
    if w == what_aim {
        // The tilt leans the ring towards its turn, so the lean goes round as the ring turns.
        let o = max(params.orbit, 1.0);
        let lap = (params.whole - o * floor(params.whole / o) + params.phase) / o;
        let way = select(-1.0, 1.0, hash(n, 5u) < 0.5);
        let yaw = TAU * (hash(n, 7u) + way * lap);
        let pitch = params.tilt * DEG * (0.4 + 0.6 * hash(n, 9u));
        return vec4f(sin(yaw) * cos(pitch), cos(yaw) * cos(pitch), sin(pitch), 1.0);
    }
    let on = has_ring(n) && q > 0.001;
    if w == what_scale {
        // Fading out, a ring widens to three times its size, out where the stage's distance
        // fade has taken it, and flattens.
        let r = mix(params.inner, params.outer, hash(n, 13u)) * (1.0 + 2.0 * (1.0 - q));
        let h = params.band * smoothstep(0.0, 0.3, q);
        return select(vec4f(0.0, 0.0, 0.0, 1.0), vec4f(r, r, h, 1.0), on);
    }
    // The storeys' colour, some rings drawn towards the posts' colour.
    let t = clamp(params.warmth, 0.0, 1.0);
    let storey = mix(vec3f(1.0, 0.62, 0.32), vec3f(0.55, 0.8, 1.0), t);
    let post = mix(vec3f(0.85, 0.42, 0.62), vec3f(0.45, 0.55, 1.0), t);
    let tint = mix(storey, post, 0.6 * hash(n, 17u));
    let fall = 1.0 - clamp(params.beat, 0.0, 1.0);
    let flash = 1.0 + params.pulse * fall * fall * fall * fall;
    let bass = clamp(params.bass, 0.0, 1.0);
    let light = tint * 1.4 * params.brightness * (0.55 + 0.5 * bass) * flash * q;
    return vec4f(select(vec3f(0.0), light, on), 1.0);
}
