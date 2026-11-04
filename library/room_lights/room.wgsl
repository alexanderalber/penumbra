// The lights of a room for a music stage, as one picture for its columns: `columns` bands side
// by side, band k for column k counted from the left, each from the floor (bottom) to the top.
// Three patterns in the palette's accent and deep colour, mixed by their shares: stripes that
// rise up the columns, every column flashing on the beat, and the light running from one column
// to the next on each beat.
// @param columns int = 4 1..16 -- columns side by side in the picture, one band each
// @param bands float = 6 1..32 -- stripes up a column
// @param rise float = 0.25 0..2 -- stripes the pattern rises per beat travelled
// @param tail float = 1.5 0.1..8 -- columns the chase's light trails behind it
// @param stripes float = 1 0..1 -- share of the stripes
// @param pulse float = 0 0..1 -- share of the pulse
// @param chase float = 0 0..1 -- share of the chase
// @param flash float = 0 0..1 -- 1 on the beat, falling to 0 before the next one
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param phase float = 0 0..1 -- how far into the beat (beat of a chan.beat_grid)
// @param beat_index float = 0 0..100000 -- beats counted (beat_index of a chan.beat_grid)
// @param bar_index float = 0 0..100000 -- bars counted (bar_index of a chan.beat_grid)
// @param phrase_bar float = 0 0..7 -- bar within the 8-bar phrase (phrase_bar of a chan.beat_grid)
// @param phrase float = 0 0..1 -- how far into the phrase (phrase of a chan.beat_grid)
// @param build float = 0 0..1 -- the build before the next section (build of a chan.beat_grid)
// @param intensity float = 1 0..1 -- how bright the lights are
// @param red float = 1 0..1 -- the accent colour
// @param green float = 0.38 0..1
// @param blue float = 0.05 0..1
// @param red2 float = 0.85 0..1 -- the deep colour
// @param green2 float = 0.05 0..1
// @param blue2 float = 0.3 0..1

fn wrap(x: f32, n: f32) -> f32 {
    return x - n * floor(x / n);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let n = max(params.columns, 1);
    let nf = f32(n);
    let x = clamp(v.uv.x, 0.0, 0.999999) * nf;
    let k = f32(min(i32(floor(x)), n - 1));
    let up = 1.0 - v.uv.y;
    let accent = clamp(vec3f(params.red, params.green, params.blue), vec3f(0.0), vec3f(1.0));
    let deep = clamp(vec3f(params.red2, params.green2, params.blue2), vec3f(0.0), vec3f(1.0));
    let flash = clamp(params.flash, 0.0, 1.0);
    let build = clamp(params.build, 0.0, 1.0);
    let phrase = clamp(params.phrase, 0.0, 1.0);
    // Which phrase this is, so the chase turns round at every new one.
    let phrases = floor((params.bar_index - params.phrase_bar) / 8.0 + 0.5);
    let forward = wrap(phrases, 2.0) < 0.5;

    // Stripes: they rise with the world, neighbouring columns half a stripe apart, and widen
    // over the phrase.
    let f = fract(up * max(params.bands, 1.0) - params.beats * params.rise + 0.5 * k);
    let half_width = mix(0.18, 0.4, phrase);
    let soft = 0.04;
    let line = 1.0 - smoothstep(half_width - soft, half_width + soft, abs(f - 0.5));
    let striped = mix(deep * 0.3, accent, line) * (0.75 + 0.25 * flash);

    // Pulse: every column lights up on the beat, brightest at the floor. Neighbouring columns
    // and every other beat take turns between the accent and the deep colour; the phrase's first
    // beat is nearly white.
    let odd = wrap(params.beat_index + k, 2.0) > 0.5;
    let hit = 1.0 - smoothstep(0.0, 0.03, phrase);
    let tone = mix(select(accent, mix(accent, deep, 0.6), odd), vec3f(1.0), 0.6 * hit);
    let pulsed = tone * (0.08 + 0.92 * max(flash, hit)) * (0.6 + 0.4 * (1.0 - up));

    // Chase: one column at a time, a step per beat, two in the build's second half; the light
    // trails behind it and the direction turns at every phrase.
    let speed = select(1.0, 2.0, build > 0.5);
    let head = wrap(floor((params.beat_index + clamp(params.phase, 0.0, 1.0)) * speed), nf);
    let behind = select(wrap(k - head, nf), wrap(head - k, nf), forward);
    let trail = exp(-behind / max(params.tail, 0.1));
    let chased = mix(deep, accent, select(0.0, 1.0, behind < 0.5)) * trail * (0.7 + 0.3 * flash);

    var rgb = striped * clamp(params.stripes, 0.0, 1.0) + pulsed * clamp(params.pulse, 0.0, 1.0)
        + chased * clamp(params.chase, 0.0, 1.0);
    rgb = rgb * (0.12 + 0.88 * clamp(params.intensity, 0.0, 1.0)) * (1.0 + 0.4 * build);
    return vec4f(rgb, 1.0);
}
