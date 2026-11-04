// The veils of mid_smoke, one texel per veil, for three geo.tex_instance nodes, one per veil model
// (veil1.obj to veil3.obj, each showing another third of smoke.wgsl's picture). The veils hang low
// over the water across the lane, `spacing` metres apart, fixed to the floor: they come towards
// the camera with it, and the camera flies through them. Each drifts slowly to one side as it
// comes. Each always turns its face to the camera. A veil fades in far off and fades out in the
// last metres before the camera.
// in0: the stage's lane picture (its `lane` output): red the lane's x, blue the metres per beat,
// texel i covering y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight
// lane at x = 0 and `step` metres per beat.
// `what` place: the foot of the veil's middle (rgb). aim: its face's direction. scale: its width
// and height, 0 where the veil shows another model (`kind`) or none. color: its light, as a
// factor on the smoke picture. Render each 4 x 3.
// @param what choice(place|aim|scale|color) = place -- what this node writes
// @param kind int = 1 1..3 -- scale: the veil model this node scales (the others get 0)
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param swell float = 0 0..1 -- the stage's swell: the smoke thickens with it
// @param dawn float = 0 0..1 -- the stage's dawn: more of the band's light towards 1
// @param presence float = 1 0..1 -- how many veils hang: towards 0 they fade out one after another, 0 none
// @param brightness float = 1.6 0..4 -- the smoke's light
// @param calm float = 0.45 0..1 -- share of the light left without swell
// @param veils int = 12 1..12 -- veils at a time
// @param spacing float = 3 1..10 -- metres between two veils along the lane
// @param wide float = 9 1..30 -- a veil's width, metres, on average
// @param tall float = 2.4 0.3..8 -- a veil's height, metres, on average
// @param spread float = 3 0..15 -- metres a veil's middle lies from the lane's at most
// @param wind float = 0.04 0..0.5 -- metres a veil drifts sideways per metre it comes nearer
// @param fade float = 5 0.5..20 -- metres before the camera where a veil starts to fade out (gone at a fifth of it)
// @param start float = 14 0..60 -- metres ahead where the far veils start to fade
// @param end float = 38 1..80 -- metres ahead where nothing is left of them
// @param near float = -1 -8..10 -- where a veil leaves, behind the camera
// @param seed int = 0 0..1000 -- other places and sizes
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

struct Veil {
    at: vec3f,
    size: vec3f,
    kind: u32,
    light: f32,
}

fn rand(n: i32, k: u32, salt: u32) -> vec3f {
    let s = u32(max(params.seed, 0));
    return vec3f(pn_hash(vec3u(bitcast<u32>(n), k + 31u * s, salt))) / 4294967296.0;
}

fn veil(i: u32) -> Veil {
    var w: Veil;
    w.size = vec3f(0.0);
    w.at = vec3f(0.0);
    w.kind = 0u;
    w.light = 0.0;
    let count = u32(clamp(params.veils, 1, 12));
    if i >= count {
        return w;
    }
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let spacing = max(params.spacing, 0.5);
    // Veil n stands at y = spacing * n - the metres travelled; the whole and the fraction of that
    // count are kept apart to stay exact far into a file. Texel i holds the i-th veil from the
    // nearest one.
    let travel = params.beats * step / spacing;
    let whole = floor(travel);
    let part = travel - whole;
    let first = ceil(params.near / spacing + part);
    let k = first + f32(i);
    let n = i32(whole + k);
    let y = spacing * (k - part);
    let r = rand(n, 7u, 41u);
    let q = rand(n, 9u, 43u);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(0.0, textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0).r, wired);
    let far = params.near + spacing * f32(count);
    let side = select(-1.0, 1.0, q.x < 0.5);
    let x = lane + params.spread * (2.0 * r.x - 1.0) + side * params.wind * (far - y);
    w.at = vec3f(x, y, -0.05 - 0.1 * r.y);
    // Width and height vary; every second one is mirrored, so six looks come from three veils.
    let mirror = select(-1.0, 1.0, q.y < 0.5);
    w.size = vec3f(mirror * params.wide * (0.7 + 0.6 * r.z), 1.0, params.tall * (0.7 + 0.6 * q.z));
    w.kind = 1u + u32(floor(r.y * 2.999));

    // The light: the swell thickens the smoke; it fades in far off and out before the camera.
    let swell = clamp(params.swell, 0.0, 1.0);
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let f = max(params.fade, 0.1);
    let close = smoothstep(0.2 * f, f, y);
    let distant = 1.0 - smoothstep(params.start, max(params.end, params.start + 0.1), y);
    let edge = smoothstep(far, far - 2.0 * spacing, y);
    // Lit from behind by the band: a near veil has less of it behind than a far one.
    let backlit = mix(0.5, 1.0, smoothstep(4.0, 18.0, y));
    let own = clamp((clamp(params.presence, 0.0, 1.0) * 1.25 - q.z * 0.999) * 4.0, 0.0, 1.0);
    w.light = params.brightness * mix(params.calm, 1.0, swell) * mix(0.6, 1.0, dawn) * close * distant * edge * own * backlit
        * (0.75 + 0.5 * r.z);
    if params.presence < 0.001 {
        w.light = 0.0;
        w.size = vec3f(0.0);
    }
    return w;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let cell = vec2u(v.pos.xy);
    let w = veil(cell.y * 4u + cell.x);
    if params.what == what_place {
        return vec4f(w.at, 1.0);
    }
    if params.what == what_aim {
        // Its face to the camera, kept upright.
        let to = vec2f(w.at.x, w.at.y);
        return vec4f(select(vec3f(0.0, 1.0, 0.0), vec3f(normalize(to), 0.0), to.y > 0.3), 1.0);
    }
    if params.what == what_scale {
        return vec4f(select(vec3f(0.0), w.size, w.kind == u32(params.kind) && w.light > 0.0), 1.0);
    }
    return vec4f(vec3f(w.light), 1.0);
}
