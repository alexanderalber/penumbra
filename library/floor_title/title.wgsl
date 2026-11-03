// The track's title in huge letters lying on the floor of the synthwave stage, across the lane,
// for the camera to fly over. At the start of a track the first copy lies `ahead` metres ahead
// and one letter appears with each beat; it keeps its distance until the last letter is there,
// then stays behind on the floor and the camera flies over it. Further copies follow every
// `every` bars of travel, all their letters there. The letters follow the lane's bends, sink
// into gaps in the floor and fade out far ahead. The picture covers the floor from `left` to `right`
// across and `near` to `far` ahead; floor_title lays it on a flat sheet just above the grid.
// Empty floor is transparent, so the grid shows through it.
// in0: the stage's lane picture (its `lane` output): red the lane's x, green its sideways slope
// per metre ahead, blue the metres per beat, alpha the floor (0 in a gap), texel i covering
// y = -8 + i * 0.25 to -8 + (i + 1) * 0.25 metres ahead. Unconnected: a straight lane at x = 0,
// no gaps and `step` metres per beat.
// in1: the title written by tex.text in a monospaced font, flush left at `pen`. in2: the same
// for the track's number. in3: measure.wgsl over in1 and in2 (where the writing begins and ends).
// Out: colour premultiplied by alpha, as pictures between nodes are. Render it about 1280 x 1024.
// @param source choice(text|track) = text -- what is written: in1 (the title) or in2 (the track's number)
// @param track_bars float = 0 0..100000 -- bars since the track began (track_bars of a chan.beat_grid)
// @param beats_per_bar float = 4 2..8 -- beats in a bar
// @param presence float = 1 0..1 -- how much is written: from 0.4 every letter (brighter towards 1), below it the letters go out one after another, 0 none
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param ahead float = 4 2..12 -- metres ahead where the first copy lies while its letters appear
// @param every float = 8 1..64 -- bars of travel from one copy to the next
// @param long float = 4 0.5..10 -- metres a letter reaches along the floor (the floor's slant makes it look shorter)
// @param wide float = 0.9 0.2..4 -- metres across from one letter to the next
// @param span float = 8 2..20 -- metres across the whole title takes at most; a long title gets narrower letters
// @param pen float = 0.02 0..0.5 -- where tex.text starts writing, as a share of its picture's width
// @param glyph float = 0.0586 0.005..0.5 -- the font's advance from one letter to the next, as a share of the text picture's width
// @param brightness float = 1.4 0..4 -- how bright the letters are
// @param rim float = 1 0..4 -- how bright their outline is, against the fill
// @param left float = -10 -20..0 -- the picture's left edge, metres across
// @param right float = 10 0..20 -- its right edge
// @param near float = -2 -8..10 -- its near edge, metres ahead
// @param far float = 14 1..40 -- its far edge
// @param fade float = 7 0..40 -- metres ahead where the letters start to fade out
// @param gone float = 11 0..56 -- metres ahead where nothing is left of them
// @param step float = 0.5 0.1..2 -- metres per beat while no lane picture is wired; with one, the stage's step

const LANE_NEAR: f32 = -8.0;
const LANE_FAR: f32 = 56.0;

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

fn ink(uv: vec2f) -> f32 {
    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
        return 0.0;
    }
    if params.source == source_track {
        return textureSampleLevel(in2, pn_clamp, uv, 0.0).a;
    }
    return textureSampleLevel(in1, pn_clamp, uv, 0.0).a;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let x = mix(params.left, params.right, v.uv.x);
    let y = mix(params.far, params.near, v.uv.y);
    let lane0 = textureLoad(in0, vec2i(0, 0), 0);
    let wired = lane0.b > 0.0;
    let step = select(params.step, lane0.b, wired);
    let u = (y - LANE_NEAR) / (LANE_FAR - LANE_NEAR);
    let lane = select(vec4f(0.0, 0.0, 0.0, 1.0), textureSampleLevel(in0, pn_clamp, vec2f(u, 0.5), 0.0), wired);
    let bpb = max(params.beats_per_bar, 1.0);
    let beats = max(params.track_bars, 0.0) * bpb;
    // Where the writing lies in the text picture, and how many letters it has.
    let m = textureLoad(in3, vec2i(0, 0), 0);
    let edges = select(m.rg, m.ba, params.source == source_track);
    let glyph = max(params.glyph, 0.001);
    let pen = params.pen;
    let count = max(round((edges.y - pen) / glyph), 1.0);
    // Which copy lies here: copy k lies `ahead + k * every` metres ahead while the first one is
    // written, a letter per beat; then all of them stay behind on the floor.
    let gap = max(params.every * bpb * step, 0.5);
    let along = y - params.ahead + step * max(beats - count, 0.0);
    let k = floor(along / gap);
    let local = along - k * gap;
    let long = max(params.long, 0.1);
    if k < 0.0 || local > long {
        return vec4f(0.0);
    }
    let wide = min(max(params.wide, 0.05), max(params.span, 0.5) / count);
    // Across: centred on the lane; along: the letter's top lies far, its foot near.
    let middle = pen + 0.5 * count * glyph;
    let tu = middle + (x - lane.r) / wide * glyph;
    let tv = 1.0 - local / long;
    let c = ink(vec2f(tu, tv));
    // A ring of samples a few pixels out: where the ink ends lies the outline.
    let size = vec2f(textureDimensions(in1));
    let o = vec2f(2.0 / max(size.x, 1.0), 2.0 / max(size.y, 1.0));
    let around = max(max(ink(vec2f(tu + o.x, tv)), ink(vec2f(tu - o.x, tv))), max(ink(vec2f(tu, tv + o.y)), ink(vec2f(tu, tv - o.y))));
    let low = min(min(ink(vec2f(tu + o.x, tv)), ink(vec2f(tu - o.x, tv))), min(ink(vec2f(tu, tv + o.y)), ink(vec2f(tu, tv - o.y))));
    let edge = clamp(around - low, 0.0, 1.0);
    let a0 = max(c, edge);
    if a0 < 0.004 {
        return vec4f(0.0);
    }
    // The letter's number: it appears on beat `i` of the track, with a short flash.
    let i = floor((tu - pen) / glyph);
    let age = beats - i;
    if age < 0.0 && k < 0.5 {
        return vec4f(0.0);
    }
    let shown = select(1.0, smoothstep(0.0, 0.1, age), k < 0.5);
    let flash = select(0.0, exp(-3.0 * max(age, 0.0)), k < 0.5);
    // Presence takes the letters out one after another, each at its own share, all of them
    // below 0.4; above it they only grow brighter, so a calm section still shows the whole title.
    let r = f32(pn_hash(vec3u(u32(max(i, 0.0)), 5u, 9u)).x) / 4294967296.0;
    let pr = clamp(params.presence, 0.0, 1.0);
    let own = smoothstep(0.3 * r, 0.3 * r + 0.1, pr) * mix(0.6, 1.0, smoothstep(0.4, 1.0, pr));
    let far = 1.0 - smoothstep(params.fade, params.gone, length(vec2f(x, y)));
    let keep = shown * own * far * clamp(lane.a, 0.0, 1.0);
    if a0 * keep < 0.004 {
        return vec4f(0.0);
    }
    // Colour: the palette from the first letter to the last, a fill darker at the letter's foot,
    // the outline bright.
    let hue = palette(clamp(i / max(count - 1.0, 1.0), 0.0, 1.0));
    let fill = hue * mix(0.3, 1.0, tv) * c;
    let line = hue * params.rim * edge;
    let light = (fill + line) * params.brightness * (1.0 + 2.0 * flash);
    return vec4f(light * keep, a0 * keep);
}
