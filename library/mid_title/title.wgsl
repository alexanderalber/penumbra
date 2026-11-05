// Lettering that stands at one place over the lake, for geo.tex_instance: each node renders 1 x 1.
// Everything here is a function of the music's position, so a seek lands the title where it
// would have been, and nothing needs remembering.
// The title appears `at` seconds into the file, `distance` metres ahead of the camera, and rises out
// of the water. It stays put in the world. The camera moves on `step` metres per beat of `travelled`,
// so the title comes nearer and passes over the camera.
// `travelled` counts from the file's start at a nearly steady pace, so travelled * (1 - at / seconds)
// gives the beats since the title appeared, without knowing what travelled was then.
// `what` place: where the lettering goes (rgb). size: its scale, 0 while it is not there.
// rim: the light on its outlines. glow: the light in its face.
// @param what choice(place|size|rim|glow) = place -- what this node writes
// @param seconds float = 0 0..100000 -- seconds into the file (seconds of a chan.beat_grid)
// @param travelled float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param beat float = 0 0..1 -- how far the current beat has gone (beat of a chan.beat_grid): the light pulses on its start
// @param bass float = 0 0..1 -- the bass (energy_low of a chan.beat_grid, smoothed): how strong the pulse is
// @param dawn float = 0 0..1 -- the stage's dawn: the rims catch more of the band's light towards 1
// @param swell float = 0 0..1 -- the stage's swell: the face glows brighter
// @param warmth float = 0 0..1 -- 0 copper rims, 1 jade
// @param presence float = 1 0..1 -- towards 0 the title sinks into the water and dims, 0 none
// @param at float = 3 0..100000 -- seconds into the file at which the title appears
// @param distance float = 30 1..200 -- metres ahead of the camera at which it appears
// @param above float = 4.2 0..30 -- metres above the water of the middle of the lettering
// @param side float = 0 -50..50 -- metres to the side; the camera flies along 0
// @param step float = 0.5 0.01..5 -- metres per beat the camera flies (the stage's `step`)
// @param rise float = 3 0..30 -- seconds it takes to rise out of the water
// @param sink float = 6 0..30 -- metres under `above` it starts from and sinks to
// @param behind float = 6 0..100 -- metres behind the camera at which it is gone
// @param light float = 1 0..4 -- brightness of rims and face
// @param pulse float = 0.15 0..1 -- how much the light brightens on each beat, times the bass

fn ahead() -> f32 {
    let moved = params.travelled * (1.0 - params.at / max(params.seconds, 0.001));
    return params.distance - params.step * moved;
}

// 0 before it appears, up to 1 when it has risen.
fn risen() -> f32 {
    return smoothstep(0.0, max(params.rise, 0.001), params.seconds - params.at);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let up = risen() * sqrt(clamp(params.presence, 0.0, 1.0));
    if params.what == what_place {
        return vec4f(params.side, ahead(), params.above - params.sink * (1.0 - up), 1.0);
    }
    if params.what == what_size {
        let there = params.seconds >= params.at && ahead() > -params.behind && params.presence > 0.0;
        return vec4f(vec3f(select(0.0, 1.0, there)), 1.0);
    }
    let warm = clamp(params.warmth, 0.0, 1.0);
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let beat = params.pulse * clamp(params.bass, 0.0, 1.0) * exp(-5.0 * clamp(params.beat, 0.0, 1.0));
    let lit = params.light * up * up * (1.0 + beat);
    if params.what == what_rim {
        let tone = mix(vec3f(1.0, 0.5, 0.2), vec3f(0.45, 1.0, 0.75), warm);
        return vec4f(tone * lit * (0.9 + 0.3 * dawn + 0.6 * swell), 1.0);
    }
    return vec4f(vec3f(lit * (0.55 + 0.3 * dawn + 1.0 * swell)), 1.0);
}
