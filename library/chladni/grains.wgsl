// One step of every sand grain. A grain jumps at random, the more the plate moves under it, and
// slides towards stiller places; so grains leave the moving parts and gather on the nodal lines.
// It runs 4 steps per frame times the component's speed (iterations), format 32f.
// in0: the grains, in1: `field` (r: amplitude, b: on the plate), in2: `density_loop` (grains per
// pixel, blurred, from the frame before; its size is the size of the sand picture), in3: `pick`
// (g: when the figure last changed).
// State of a grain: r, g = its position in pixels of the sand picture, from the top left;
// b = -1 while it is not on the plate yet, 0 on it; a = how far it moved per step lately, in
// pixels (for the colour of moving sand and the output motion).
// A grain not on the plate yet is sprinkled on at a random place of the plate.
// @param level float = 1 0..2 -- how hard the plate is played; bind it to the music's level. 0: the sand rests
// @param kick float = 0 0..1 -- a hit on the plate that throws every grain up; bind it to an envelope of the beat
// @param toss float = 1 0..1 -- a short kick at every new figure, so the sand leaves the old lines at once
// @param toss_time float = 0.25 0..1 -- seconds the toss at a new figure lasts
// @param hop float = 4 0..20 -- pixels a grain jumps per step, at random, where the plate moves most
// @param drift float = 3 0..10 -- pixels per step a grain slides towards stiller places, where the plate moves most
// @param kick_hop float = 18 0..60 -- pixels a grain jumps per step at kick 1, anywhere on the plate
// @param jitter float = 0.35 0..2 -- pixels of jiggle everywhere while the plate plays; keeps the lines from breaking into dots
// @param spread float = 0.15 0..0.5 -- grains move away from crowded pixels (in2), so the lines fill evenly
// @param now float = 0 -- follows the component's clock (seconds since the start)

fn rnd(h: u32) -> f32 {
    return f32(h >> 8u) / 16777216.0;
}

fn amp(pos: vec2f, world: vec2f) -> f32 {
    return abs(textureSampleLevel(in1, pn_clamp, pos / world, 0.0).r);
}

fn on_plate(pos: vec2f, world: vec2f) -> bool {
    let inside = all(pos >= vec2f(0.0)) && all(pos < world);
    return inside && textureSampleLevel(in1, pn_clamp, pos / world, 0.0).b > 0.9;
}

fn dens(pos: vec2f, world: vec2f) -> f32 {
    return textureSampleLevel(in2, pn_clamp, pos / world, 0.0).r;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    let s = textureLoad(in0, p, 0);
    let world = vec2f(textureDimensions(in2));
    // Random numbers only: the show's frame counter does not restart, and need not.
    let nstep = u32(pn_time.frame) * u32(max(pn.iterations, 1)) + u32(pn.iteration);
    let h = pn_hash(vec3u(vec2u(p), nstep));
    let h2 = pn_hash(h);
    let tau = 6.2831853;

    if s.b < 0.0 {
        // Sprinkle: up to 8 tries for a place on the plate per step.
        var hh = h;
        for (var i = 0; i < 8; i++) {
            hh = pn_hash(hh);
            let c = vec2f(rnd(hh.x), rnd(hh.y)) * world;
            if on_plate(c, world) {
                return vec4f(c, 0.0, 0.0);
            }
        }
        return vec4f(0.0, 0.0, -1.0, 0.0);
    }
    var pos = s.rg;
    var moved = s.a;
    let lv = params.level;
    let changed = textureLoad(in3, vec2i(0, 0), 0).g;
    var kick = params.kick;
    if params.now - changed < params.toss_time {
        kick = max(kick, params.toss);
    }
    let a = min(amp(pos, world), 1.5);
    // Slope of the amplitude, 2 pixels each way.
    let e = 2.0;
    let g = vec2f(amp(pos + vec2f(e, 0.0), world) - amp(pos - vec2f(e, 0.0), world),
                  amp(pos + vec2f(0.0, e), world) - amp(pos - vec2f(0.0, e), world)) / (2.0 * e);
    let gl = length(g);
    var mv = vec2f(0.0);
    if gl > 1e-5 {
        mv -= lv * params.drift * a * g / gl;
    }
    let ang = rnd(h.x) * tau;
    let len = rnd(h.y) * lv * (params.hop * a + params.jitter);
    mv += len * vec2f(cos(ang), sin(ang));
    let ang2 = rnd(h.z) * tau;
    mv += rnd(h2.x) * kick * params.kick_hop * vec2f(cos(ang2), sin(ang2));
    // Away from crowded pixels.
    let dg = vec2f(dens(pos + vec2f(e, 0.0), world) - dens(pos - vec2f(e, 0.0), world),
                   dens(pos + vec2f(0.0, e), world) - dens(pos - vec2f(0.0, e), world)) / (2.0 * e);
    var push = -params.spread * lv * dg;
    let pl = length(push);
    if pl > 1.0 {
        push = push / pl;
    }
    mv += push;
    // A jump that would leave the plate is refused: the grain stays.
    let next = pos + mv;
    if on_plate(next, world) {
        pos = next;
        moved = mix(moved, length(mv), 0.2);
    } else {
        moved = mix(moved, 0.0, 0.2);
    }
    // A plate that shrank (a new shape input) leaves grains off it: they are sprinkled again.
    if !on_plate(pos, world) {
        return vec4f(0.0, 0.0, -1.0, 0.0);
    }
    return vec4f(pos, 0.0, moved);
}
