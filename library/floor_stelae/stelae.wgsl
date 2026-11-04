// Stelae on the turns of the valley stage's lane, drawn by three geo.tex_instance nodes, one per
// glyph (stela_1.obj to stela_3.obj, stelae.py). One texel per stela, a row each: stela k stands
// at the k-th turn of the lane ahead of `near`, on the outside of the turn, its front towards the
// lane and turned towards the camera. A turn is where the lane stops swinging one way and starts
// back: where its sideways slope changes sign. So the stelae follow the stage's turns whatever
// its `bars_per_turn`, and each keeps its place on the floor as it comes nearer.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the shore (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: no lane, a stela every
// `every` beats on alternate sides.
// `what` place: where the model's origin goes (rgb). aim: where its front points. glyph1 to
// glyph3: the scale of stela_k.obj, 0 where the stela shows another glyph or none stands. color:
// the light on its rim and glyph. Render each 1 x 8.
// @param what choice(place|aim|glyph1|glyph2|glyph3|color) = place -- what this node writes
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the light pulses on its start
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): how strong the pulse is
// @param dawn float = 0 0..1 -- the stage's dawn: the stelae catch more of the band's light towards 1
// @param swell float = 0 0..1 -- the stage's swell: the band glows and lights the stelae more
// @param warmth float = 0 0..1 -- 0 copper light, 1 jade
// @param presence float = 1 0..1 -- how many stelae stand: towards 0 they sink into the water one after another, 0 none
// @param seed int = 0 0..1000 -- other glyphs and sizes for the stelae
// @param tall float = 2.4 0.5..6 -- a stela's height, metres, on average
// @param wide float = 0.9 0.2..3 -- a stela's width, metres
// @param thick float = 0.35 0.1..2 -- a stela's depth, metres
// @param light float = 1 0..4 -- brightness of the light on rim and glyph
// @param pulse float = 0.25 0..1 -- how much the light brightens on each beat, times the bass
// @param face float = 30 0..90 -- degrees a stela's front turns from the lane towards the camera
// @param lane float = 1.6 0..5 -- half the width of the free lane
// @param clear float = 1.3 0..8 -- metres between the lane's edge and a stela's front
// @param near float = -4 -8..10 -- where the stelae leave, behind the camera
// @param far float = 36 1..56 -- where they appear
// @param every float = 16 2..64 -- beats between two stelae while no lane picture is wired
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;
const TEXELS: i32 = 256;
// stelae.py: the base below the water line, as a share of the height.
const BASE: f32 = 0.05;

fn on_floor(y: f32, step: f32) -> bool {
    let s = max(step, 0.01);
    let c = params.beats + y / s;
    let mid = (floor(c + 0.01) + 0.5 - params.beats) * s;
    let texel = clamp(i32(floor((mid - LANE_NEAR) / (LANE_FAR - LANE_NEAR) * 256.0)), 0, 255);
    return textureLoad(in0, vec2i(texel, 0), 0).a >= 0.5;
}

struct Stela {
    at: vec3f,
    aim: vec3f,
    size: vec3f,
    glyph: u32,
}

// The k-th turn of the lane at or beyond `near`: its distance ahead, or a negative number past
// the last one in view.
fn turn_at(k: u32) -> f32 {
    var found = 0u;
    var before = textureLoad(in0, vec2i(0, 0), 0).g;
    for (var i = 1; i < TEXELS; i++) {
        let now = textureLoad(in0, vec2i(i, 0), 0).g;
        if (before < 0.0) != (now < 0.0) && before != now {
            // Where the slope crosses zero between the two texel centres.
            let f = before / (before - now);
            let y = LANE_NEAR + (f32(i) - 0.5 + f) * (LANE_FAR - LANE_NEAR) / f32(TEXELS);
            if y >= params.near {
                if found == k {
                    return y;
                }
                found += 1u;
            }
        }
        before = now;
    }
    return -100.0;
}

fn stela(k: u32) -> Stela {
    var s: Stela;
    s.size = vec3f(0.0);
    s.aim = vec3f(0.0, -1.0, 0.0);
    s.glyph = 0u;
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    var y: f32;
    var lane = vec4f(0.0, 0.0, step, 1.0);
    var sign: f32;
    if wired {
        y = turn_at(k);
        if y < params.near {
            return s;
        }
        let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
        lane = textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0);
        // The outside of the turn: the side the lane has swung to.
        let around = textureSampleLevel(in0, pn_clamp, vec2f(u + 8.0 / (LANE_FAR - LANE_NEAR), 0.5), 0.0);
        sign = select(-1.0, 1.0, lane.r >= around.r);
    } else {
        let gap = max(params.every, 1.0) * step;
        let travel = params.beats * step / gap;
        let part = fract(travel);
        let first = ceil(params.near / gap + part);
        y = gap * (first + f32(k) - part);
        sign = select(-1.0, 1.0, (i32(floor(travel) + first + f32(k)) & 1) == 0);
    }
    // The stela's own number: the distance travelled when its place passes the camera, in bars.
    let c = params.beats + y / max(step, 0.01);
    let n = i32(floor(c / 4.0 + 0.5));
    let r = vec3f(pn_hash(vec3u(bitcast<u32>(n), u32(max(params.seed, 0)), 61u))) / 4294967296.0;
    let tall = params.tall * mix(0.8, 1.2, r.x);
    let wide = params.wide * mix(0.85, 1.15, r.y);
    s.glyph = 1u + min(u32(r.z * 3.0), 2u);
    let turn = radians(params.face);
    s.aim = vec3f(-sign * cos(turn), -sin(turn), 0.0);
    let r2 = vec3f(pn_hash(vec3u(bitcast<u32>(n), u32(max(params.seed, 0)), 67u))) / 4294967296.0;
    // Presence: each stela sinks at its own share of it; at a steady presence it stands whole
    // or not at all, so no stubs stick out of the water.
    let own = clamp((params.presence * 1.6 - r2.x) * 8.0, 0.0, 1.0);
    let up = own * own * (3.0 - 2.0 * own);
    let off = params.lane * (1.0 + 0.5 * abs(lane.g)) + params.clear + 0.5 * params.thick;
    s.at = vec3f(lane.r + sign * off, y, -(1.0 + BASE) * tall * (1.0 - up));
    if y > params.far || up < 0.001 || (wired && !on_floor(y, step)) {
        return s;
    }
    s.size = vec3f(wide, params.thick, tall);
    return s;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let s = stela(u32(v.pos.y));
    if params.what == what_place {
        return vec4f(s.at, 1.0);
    }
    if params.what == what_aim {
        return vec4f(s.aim, 1.0);
    }
    if params.what == what_glyph1 {
        return vec4f(select(vec3f(0.0), s.size, s.glyph == 1u), 1.0);
    }
    if params.what == what_glyph2 {
        return vec4f(select(vec3f(0.0), s.size, s.glyph == 2u), 1.0);
    }
    if params.what == what_glyph3 {
        return vec4f(select(vec3f(0.0), s.size, s.glyph == 3u), 1.0);
    }
    // The light from the band: more towards dawn and in the swell, a small pulse on each beat.
    let b = clamp(params.beat, 0.0, 1.0);
    let kick = smoothstep(0.0, 0.06, b) * (1.0 - b) * (1.0 - b) * (1.0 - b);
    let bass = clamp(params.bass, 0.0, 1.0);
    let level = params.light * (0.25 + 0.35 * clamp(params.dawn, 0.0, 1.0) + 0.6 * clamp(params.swell, 0.0, 1.0)
        + params.pulse * kick * (0.3 + bass));
    let tone = mix(vec3f(0.60, 0.24, 0.075), vec3f(0.10, 0.40, 0.28), clamp(params.warmth, 0.0, 1.0));
    return vec4f(tone * level, 1.0);
}
