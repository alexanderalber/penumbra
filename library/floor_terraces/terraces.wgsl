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
// color: the rim light's colour. Render each 4 x 12.
// @param what choice(place|aim|terrace|pyramid|color) = place -- what this node writes
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

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
// terraces.py: the base below the water line, as a share of the height.
const BASE: f32 = 0.05;

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

fn place(cell: vec2u) -> Place {
    var p: Place;
    p.size = vec3f(0.0);
    p.aim = vec3f(0.0, 1.0, 0.0);
    p.pyramid = false;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let side = cell.x / 2u;
    let line = cell.x % 2u;
    if side > 1u {
        return p;
    }
    // Place n of a line stands at y = gap_y * (n - travel); the back line half a place later.
    let bpb = max(params.beats_per_bar, 1.0);
    let spacing = max(params.spacing, 0.1);
    let gap_y = spacing * bpb * step;
    let shift = 0.5 * f32(line);
    let travel = (params.beats / bpb + params.drift) / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / gap_y + part - shift);
    let k = first + f32(cell.y);
    let y = gap_y * (k - part + shift);
    let n = i32(whole + k);
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(n), cell.x, 53u))) / 4294967296.0;
    let r2 = vec3f(pn_hash(vec3u(bitcast<u32>(n), cell.x, 59u))) / 4294967296.0;
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
    // Presence: each terrace has its own share of it, so as presence falls they sink into the
    // water one after another. At a steady presence a terrace stands whole or not at all.
    let own = clamp((params.presence * 1.6 - r.z) * 8.0, 0.0, 1.0);
    let up = own * own * (3.0 - 2.0 * own);
    p.at = vec3f(lane.r + sign * off, y, -(1.0 + BASE) * height * (1.0 - up));
    if y > params.far || up < 0.001 || r2.z >= params.fill || (wired && !on_floor(y, step)) {
        return p;
    }
    p.size = vec3f(along, depth, height);
    return p;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
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
