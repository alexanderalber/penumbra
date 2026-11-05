// A rock ring in the valley stage's lake, with a pool of jade light in it and a shaft of light
// falling in, drawn by geo.tex_instance nodes. The camera flies over it. Each ring stands at a fixed
// place on the floor, given in beats of `travelled`, so a seek finds it where it was.
// The pool has seven hot spots. They flash in turn on the beat, in an order that jumps across the
// pool. Each hot spot's light is worked out from the beat alone, so nothing has to be remembered.
// in0: the stage's lane picture (its `lane` output): blue the metres per beat.
// `what` place: where the ring stands (rgb). size: its scale, 0 where none is in view. rim: the
// copper light on its edges. wall: the pool's light on its inner wall. pool: the water's brightness.
// shaft: the shaft's scale. shaft_light: its brightness. Render these 1 x 1. hot_place, hot_size,
// hot_light: the hot spots; render them 7 x 1.
// @param what choice(place|size|rim|wall|pool|shaft|shaft_light|hot_place|hot_size|hot_light) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid)
// @param beat_index float = 0 -100..100000 -- the grid's beat count (beat_index of a chan.beat_grid)
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): the flashes grow with it
// @param dawn float = 0 0..1 -- the stage's dawn: the rim catches more of the band's light towards 1
// @param swell float = 0 0..1 -- the stage's swell: the rim and the shaft brighten
// @param warmth float = 0 0..1 -- 0 copper rims, 1 jade ones; the pool stays jade
// @param at1 float = 0 0..20000 -- where the first ring stands, in beats of `travelled`; 0 none
// @param at2 float = 0 0..20000 -- the second ring
// @param at3 float = 0 0..20000 -- the third ring
// @param radius float = 5 1..20 -- the ring's radius, metres
// @param rim float = 0.6 0.1..3 -- the ring's height over the water at its highest, metres; the camera flies 0.9 to 2.4 m up
// @param side float = 0 -10..10 -- metres to the right of the camera's line where the ring's middle lies
// @param light float = 1 0..4 -- brightness of the rim, the wall and the pool
// @param flash float = 3 0..10 -- brightness of a hot spot at its flash
// @param idle float = 0.12 0..1 -- brightness of a hot spot between its flashes
// @param decay float = 1.5 0.3..6 -- how fast a flash dies away, per beat
// @param shaft_height float = 18 2..60 -- the shaft's height, metres
// @param shaft_light float = 0.5 0..4 -- the shaft's brightness; 0 none
// @param near float = -14 -40..0 -- where a ring leaves, behind the camera
// @param far float = 42 1..100 -- where a ring appears

const HOTS: f32 = 7.0;
// A hot spot's place and size in the pool: the golden angle keeps neighbours in order apart.
const GOLDEN: f32 = 2.3999632;
const HOT_REACH: f32 = 0.72;
const HOT_SIZE: f32 = 0.62;
const HOT_Z: f32 = 0.07;

// The nearest ring beyond `near`, metres ahead; a large number when there is none.
fn site() -> f32 {
    let step = max(textureLoad(in0, vec2i(0, 0), 0).b, 0.01);
    var best = 1e6;
    let ats = vec3f(params.at1, params.at2, params.at3);
    for (var i = 0; i < 3; i++) {
        if ats[i] > 0.0 {
            let y = (ats[i] - params.beats) * step;
            if y >= params.near && y < best {
                best = y;
            }
        }
    }
    return best;
}

fn kick() -> f32 {
    let b = clamp(params.beat, 0.0, 1.0);
    return smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
}

// The light of hot spot i. Spot i flashes on every seventh beat; 5 i modulo 7 sets which, so one
// spot's flash is followed by a spot three places on, and never by its neighbour.
fn hot_level(i: f32) -> f32 {
    let on = i * 5.0 - HOTS * floor(i * 5.0 / HOTS);
    let since = params.beat_index - on;
    let age = since - HOTS * floor(since / HOTS) + clamp(params.beat, 0.0, 1.0);
    let flash = smoothstep(0.0, 0.05, age) * exp(-age * params.decay);
    return params.idle + params.flash * flash * (0.65 + 0.7 * clamp(params.bass, 0.0, 1.0));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let y = site();
    let in_view = y <= params.far;
    let i = floor(v.pos.x);
    if params.what == what_place {
        return vec4f(params.side, y, 0.0, 1.0);
    }
    if params.what == what_size {
        return vec4f(select(vec3f(0.0), vec3f(params.radius, params.radius, params.rim), in_view), 1.0);
    }
    if params.what == what_shaft {
        let shows = in_view && y > 1.5 && params.shaft_light > 0.0;
        return vec4f(select(vec3f(0.0), vec3f(params.radius * 1.9, 1.0, params.shaft_height), shows), 1.0);
    }
    if params.what == what_hot_place {
        let r = sqrt((i + 0.5) / HOTS) * HOT_REACH * params.radius;
        let a = i * GOLDEN;
        return vec4f(params.side + r * cos(a), y + r * sin(a), HOT_Z, 1.0);
    }
    if params.what == what_hot_size {
        let s = params.radius * HOT_SIZE * (0.8 + 0.08 * i);
        return vec4f(select(vec3f(0.0), vec3f(s, s, 1.0), in_view), 1.0);
    }
    if params.what == what_hot_light {
        return vec4f(vec3f(hot_level(i)), 1.0);
    }
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let beat = kick() * (0.3 + clamp(params.bass, 0.0, 1.0));
    if params.what == what_rim {
        let warm = clamp(params.warmth, 0.0, 1.0);
        let tone = mix(vec3f(1.0, 0.5, 0.2), vec3f(0.45, 1.0, 0.75), warm);
        return vec4f(tone * params.light * (0.6 + 0.5 * dawn + 0.5 * swell), 1.0);
    }
    if params.what == what_wall {
        return vec4f(vec3f(0.1, 0.85, 0.6) * params.light * (0.22 + 0.2 * beat), 1.0);
    }
    if params.what == what_pool {
        return vec4f(vec3f(params.light * (0.8 + 0.25 * beat)), 1.0);
    }
    // what_shaft_light: fades in with distance by itself, since it stays out of the data pass, and
    // goes before the camera reaches its plane.
    let fade = (1.0 - smoothstep(0.45 * params.far, params.far, y)) * smoothstep(1.5, 10.0, y);
    return vec4f(vec3f(params.shaft_light * fade * (0.75 + 0.5 * swell + 0.15 * beat)), 1.0);
}
