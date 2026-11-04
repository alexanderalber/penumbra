// The ships of outside_ships, for geo.tex_instance: huge ships far outside stage_ascent's tower,
// crossing the view slowly from one side to the other. Each of `ships` lanes sends one ship at a
// time: it takes `cross` beats from far out on one side to far out on the other, then the lane
// rests for `rest` beats. Each ship draws its own hull (one of three), its distance between
// `near` and `far` metres, its height, its heading, its way and its length around `length` from
// the lane, the crossing's number and `seed`. As the camera climbs, the ships sink slowly: by
// `sink` times the camera's climb, so they fall behind the tower's storeys, which pass at the
// full climb. Towards `presence` 0, or where the stage lies down as a tube (the pose's `curve`
// towards 1), the ships fade into the sky.
// Ships: texel i of a `ships`-wide picture (render 6 x 1) is lane i's ship. `what` place: its
// middle. aim: its heading. size_a, size_b, size_c: its length on each axis where it has that
// hull, 0 elsewhere. edge: the colour of its edges. engine: the colour of its engines.
// Lights: texel (j, i) of a picture 10 x 6 is light j of lane i's ship. light_place: where it
// is. light_size: its size, 0 while it is off. light_color: its colour.
// @param what choice(place|aim|size_a|size_b|size_c|edge|engine|light_place|light_size|light_color) = place -- what this node writes
// @param whole float = 0 0..20000 -- the whole beats travelled (travelled of a chan.beat_grid less its fraction)
// @param phase float = 0 0..1 -- the fraction of the beat travelled
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the engines brighten with it
// @param presence float = 1 0..1 -- 1 the ships cross, towards 0 they fade into the sky
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the light of edges, engines and running lights
// @param ships int = 4 1..6 -- lanes, each with one ship at a time
// @param cross float = 384 32..4096 -- beats a ship takes from one side to the other; it is in view for about 40 % of them
// @param rest float = 256 0..4096 -- beats a lane waits before its next ship
// @param near float = 150 40..2000 -- metres to the nearest ships' paths
// @param far float = 340 40..2000 -- metres to the farthest ships' paths
// @param length float = 200 10..1000 -- metres a ship is long, on average
// @param sink float = 0.15 0..1 -- how much of the camera's climb the ships sink by: 0 they climb along, 1 they stand still in the world
// @param storey float = 3 1..10 -- metres from one storey to the next (the stage's `ceiling`)
// @param storey_beats float = 4 1..16 -- beats the camera takes to climb one storey (the stage's `storey_beats`)
// @param blink float = 2 0.25..16 -- beats from one flash of a running light to the next
// @param seed int = 0 0..1000 -- another seed gives other ships
// @param curve float = 0 0..1 -- 0 where the tower stands (curve of the stage's pose); towards 1 the ships fade out as by `presence`

const SHIPS_MAX: u32 = 6u;
const DEG: f32 = 0.017453293;

// lights begin
const LIGHTS: u32 = 10u;
// Per hull, per light: x, y, z in the model's units and the kind (0 strobe, 1 port, 2
// starboard, 3 beacon, 4 amber).
const LIGHT_AT = array<vec4f, 30>(
    vec4f(0.000, 0.500, 0.000, 0.0),
    vec4f(-0.090, 0.200, 0.000, 1.0),
    vec4f(0.090, 0.200, 0.000, 2.0),
    vec4f(-0.090, -0.270, 0.000, 1.0),
    vec4f(0.090, -0.270, 0.000, 2.0),
    vec4f(0.000, 0.260, 0.065, 3.0),
    vec4f(0.000, -0.300, 0.100, 3.0),
    vec4f(-0.080, -0.420, 0.000, 4.0),
    vec4f(0.080, -0.420, 0.000, 4.0),
    vec4f(0.000, -0.100, 0.040, 0.0),
    vec4f(0.000, 0.500, 0.000, 0.0),
    vec4f(-0.155, -0.300, -0.010, 1.0),
    vec4f(0.155, -0.300, -0.010, 2.0),
    vec4f(-0.060, 0.120, 0.000, 1.0),
    vec4f(0.060, 0.120, 0.000, 2.0),
    vec4f(0.000, -0.220, 0.100, 3.0),
    vec4f(0.000, -0.380, 0.150, 3.0),
    vec4f(-0.120, -0.500, -0.010, 4.0),
    vec4f(0.120, -0.500, -0.010, 4.0),
    vec4f(0.000, 0.250, 0.020, 0.0),
    vec4f(0.000, 0.500, 0.000, 0.0),
    vec4f(-0.215, -0.080, 0.000, 1.0),
    vec4f(0.215, -0.080, 0.000, 2.0),
    vec4f(0.000, -0.080, 0.215, 3.0),
    vec4f(0.000, -0.080, -0.215, 3.0),
    vec4f(0.000, 0.220, 0.070, 0.0),
    vec4f(-0.050, 0.100, 0.000, 1.0),
    vec4f(0.050, 0.100, 0.000, 2.0),
    vec4f(-0.060, -0.450, 0.000, 4.0),
    vec4f(0.060, -0.450, 0.000, 4.0),
);
// lights end

fn hash(lane: u32, crossing: f32, salt: u32) -> f32 {
    let s = u32(max(params.seed, 0));
    return f32(pn_hash(vec3u(lane * 7919u + s * 104729u, bitcast<u32>(i32(crossing)), salt)).x) / 4294967296.0;
}

struct Ship {
    on: bool,
    hull: u32,
    at: vec3f,
    ahead: vec3f,
    length: f32,
    distance: f32,
}

fn there() -> f32 {
    return clamp(params.presence, 0.0, 1.0) * (1.0 - clamp(params.curve, 0.0, 1.0));
}

fn ship(lane: u32) -> Ship {
    var s: Ship;
    let cross = max(params.cross, 1.0);
    let period = cross + max(params.rest, 0.0);
    let n = max(params.ships, 1);
    // The lanes start a share of the period apart, so their ships come one after another.
    let t = params.whole + params.phase + period * (f32(lane) / f32(n) + 0.37);
    let crossing = floor(t / period);
    let u = t - crossing * period;
    s.on = i32(lane) < n && u < cross;
    s.hull = min(u32(hash(lane, crossing, 1u) * 3.0), 2u);
    let d = mix(params.near, params.far, hash(lane, crossing, 2u));
    let rise = tan(mix(-3.0, 18.0, hash(lane, crossing, 3u)) * DEG);
    let heading = (hash(lane, crossing, 4u) - 0.5) * 24.0 * DEG;
    let way = select(-1.0, 1.0, hash(lane, crossing, 5u) < 0.5);
    s.length = params.length * mix(0.75, 1.3, hash(lane, crossing, 6u));
    s.ahead = way * vec3f(cos(heading), sin(heading), 0.0);
    // Far enough out on both sides that a ship comes and goes out of view at every camera preset.
    let reach = 3.0 * d + s.length;
    let along = (u / cross - 0.5) * 2.0 * reach;
    let climb = params.storey / max(params.storey_beats, 0.25);
    let sunk = params.sink * climb * (u - 0.5 * cross);
    s.at = vec3f(0.0, d, d * rise - sunk) + along * s.ahead;
    s.distance = length(s.at);
    return s;
}

// How much light is left at the ship's distance: farther ships are paler.
fn haze(s: Ship) -> f32 {
    return 1.0 - 0.5 * smoothstep(params.near, 2.0 * params.far, s.distance);
}

fn warm_cold(warm: vec3f, cold: vec3f) -> vec3f {
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let w = params.what;
    let lights = w == what_light_place || w == what_light_size || w == what_light_color;
    let lane = select(cell.x, cell.y, lights);
    let j = select(0u, cell.x, lights);
    if lane >= SHIPS_MAX || j >= LIGHTS {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
    let s = ship(lane);
    let q = there();
    let on = s.on && q > 0.001;
    if w == what_place {
        return vec4f(s.at, 1.0);
    }
    if w == what_aim {
        return vec4f(s.ahead, 1.0);
    }
    if w == what_size_a || w == what_size_b || w == what_size_c {
        let hull = select(select(2u, 1u, w == what_size_b), 0u, w == what_size_a);
        let shown = on && s.hull == hull;
        return vec4f(vec3f(select(0.0, s.length, shown)), 1.0);
    }
    let light = params.brightness * q * haze(s);
    if w == what_edge {
        let c = warm_cold(vec3f(0.85, 0.42, 0.62), vec3f(0.45, 0.55, 1.0));
        return vec4f(c * 1.4 * light, 1.0);
    }
    if w == what_engine {
        let c = warm_cold(vec3f(1.0, 0.55, 0.2), vec3f(0.3, 0.75, 1.0));
        return vec4f(c * (1.2 + 0.8 * clamp(params.bass, 0.0, 1.0)) * light, 1.0);
    }
    var table = LIGHT_AT;
    let p = table[min(s.hull * LIGHTS + j, 3u * LIGHTS - 1u)];
    if w == what_light_place {
        let right = vec3f(s.ahead.y, -s.ahead.x, 0.0);
        let local = p.x * right + p.y * s.ahead + vec3f(0.0, 0.0, p.z);
        return vec4f(s.at + s.length * local, 1.0);
    }
    // Strobes flash briefly once per `blink` beats, beacons glow for a third of it, the side
    // lights stay on, the amber ones take turns.
    let kind = u32(p.w);
    let beats = (params.whole + params.phase) / max(params.blink, 0.01) + hash(lane, f32(j), 9u);
    let f = fract(beats);
    var lit = true;
    if kind == 0u {
        lit = f < 0.08 || (f > 0.16 && f < 0.22);
    } else if kind == 3u {
        lit = fract(0.5 * beats) < 0.33;
    } else if kind == 4u {
        lit = fract(0.5 * beats + 0.5 * f32(j % 2u)) < 0.5;
    }
    if w == what_light_size {
        let size = max(0.009 * s.length, 1.0);
        return vec4f(vec3f(select(0.0, size, on && lit)), 1.0);
    }
    var c = vec3f(1.0, 1.0, 1.0);
    if kind == 1u {
        c = vec3f(1.0, 0.12, 0.1);
    } else if kind == 2u {
        c = vec3f(0.15, 1.0, 0.35);
    } else if kind == 3u {
        c = vec3f(1.0, 0.25, 0.15);
    } else if kind == 4u {
        c = warm_cold(vec3f(1.0, 0.7, 0.25), vec3f(0.5, 0.85, 1.0));
    }
    return vec4f(c * 2.5 * light, 1.0);
}
