// One step of the wave equation on a height field, run `iterations` times per frame (8 times
// the component's speed).
//
// State texture (32-bit floats): r = height now, g = height one step before, b = running mean of
// height squared (the energy view and the ripple output), a = brightness of the people input
// at the last step (for push move).
//
// The scheme (leapfrog, with the isotropic 9-point Laplacian, so rings stay round):
//   next = now + (now - before) * (1 - damping) + speed^2 * laplacian(now)
//          + soften * laplacian(now - before)
// Stable while speed^2 * 16/3 <= 4, so speed <= 0.866 texels per step; it is held at 0.85.
// The equation has no dispersion: a dent spreads as one ring of the same shape. Real drops
// leave a train of rings, so a drop here starts as a few rings already (rings).
//
// in0: the state. in1: walls (walls.wgsl): the height is held at 0 where it is bright, so waves
// reflect off it. in2: people: press holds the surface down where it is bright; move pushes
// where it changed since the last frame (a moving shape leaves a wake). in3: the small state
// (pulse.wgsl): the sources' phase, beat and calm drops, the frame time.
//
// @param pause bool = false -- holds the water as it is
// @param wave_speed float = 0.3 0.05..0.85 -- wave speed in texels per step; held at 0.85, above 0.866 it would blow up
// @param wavelength float = 24 6..120 -- of the sources, in texels
// @param push choice(off|press|move) = move -- what the people input does; off: nothing; press: holds the water down where it is bright (a person standing in it); move: pushes where it changed (a person wading leaves a wake)
// @param push_strength float = 1 0..5 -- how hard the people input pushes
// @param damping float = 0.002 0..0.05 -- share of the motion lost per step; 0.002 lets a ring fade in about 2 s at 8 steps per frame
// @param edges choice(reflect|absorb|wrap) = reflect -- reflect: a pond's rim; absorb: open water, waves leave; wrap: what leaves one side comes in at the other
// @param sponge float = 48 4..200 -- width of the absorbing band along the edges (edges absorb), in texels
// @param soften float = 0.005 0..0.2 -- damps the shortest waves only (texel-sized noise); above 0.25 it blows up
// @param average float = 0.01 0.001..0.2 -- how fast the energy view and the ripple output follow, per step
// @param rain choice(off|random|beat) = random -- off: no drops; random: drops at random places; beat: one drop at each beat of the beat input
// @param rate float = 8 0..60 -- random rain, drops per second
// @param drop_size float = 5 1..30 -- radius of a drop's middle dent, in texels; the whole drop reaches rings x drop_size
// @param rings int = 3 1..8 -- dents per drop: 1 is one smooth dent, more gives the train of rings a real drop leaves
// @param drop_strength float = 1.2 -3..3 -- how deep a drop pushes the surface; negative lifts it
// @param source choice(off|plane|point|two) = off -- a steady wave maker. plane: a straight wave from the left edge; point: one bobbing point; two: two points apart (interference)
// @param amplitude float = 0.4 0..1 -- of the source
// @param position vec2 = [0.2, 0.5] -- of the point source, and the middle of the two, in shares of the picture from the top left
// @param spacing float = 80 4..400 -- distance of the two points, in texels

var<private> n: vec2i;
var<private> wrap: bool;

fn edge(q: vec2i) -> vec2i {
    let wrapped = ((q % n) + n) % n;
    return select(clamp(q, vec2i(0), n - vec2i(1)), wrapped, wrap);
}

fn rand2(f: u32, k: u32) -> vec2f {
    let h = pn_hash(vec3u(f, k, 7919u));
    return vec2f(f32(h.x >> 8u), f32(h.y >> 8u)) / 16777216.0;
}

fn rand1(f: u32, k: u32) -> f32 {
    let h = pn_hash(vec3u(f, k, 104729u));
    return f32(h.z >> 8u) / 16777216.0;
}

// k dents (the middle one and k - 1 rings around it), each about 2 r wide, under a smooth window
// that ends at k r. k = 1 is one smooth dent of radius r.
fn hump(d: f32, r: f32, k: f32) -> f32 {
    let t = clamp(d / (r * k), 0.0, 1.0);
    let win = 0.5 + 0.5 * cos(3.14159265 * t);
    return win * cos(6.2831853 * (k - 1.0) * t);
}

// A drop as minus the 5-point Laplacian of the hump, scaled so its rings keep their height. The
// neighbours are taken as the step takes them (clamped or wrapped), so the drop adds up to 0
// over the whole surface: rain never raises or lowers the water level.
fn drop_at(p: vec2i, c: vec2f, r: f32, k: f32) -> f32 {
    if distance(vec2f(p), c) > r * k + 2.0 {
        return 0.0;
    }
    let w = select(3.14159265 / r, 6.2831853 * (k - 1.0) / (r * k), k > 1.5);
    let scale = 2.0 * (1.0 - cos(w));
    let h0 = hump(distance(vec2f(p), c), r, k);
    var s = 0.0;
    s += hump(distance(vec2f(edge(p + vec2i(1, 0))), c), r, k);
    s += hump(distance(vec2f(edge(p - vec2i(1, 0))), c), r, k);
    s += hump(distance(vec2f(edge(p + vec2i(0, 1))), c), r, k);
    s += hump(distance(vec2f(edge(p - vec2i(0, 1))), c), r, k);
    return -(s - 4.0 * h0) / max(scale, 1e-4);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    n = vec2i(textureDimensions(in0));
    let p = vec2i(v.pos.xy);
    let here = textureLoad(in0, p, 0);
    if params.pause != 0u {
        return here;
    }
    wrap = params.edges == edges_wrap;
    let uv = (vec2f(p) + 0.5) / vec2f(n);
    let pf = vec2f(p);
    let first = pn.iteration == 0;

    // People brightness now; it changes only between frames, so people move acts in pass 0.
    let m = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, uv, 0.0).rgb), 0.0, 1.0);

    if pn_luma(textureSampleLevel(in1, pn_clamp, uv, 0.0).rgb) > 0.5 {
        return vec4f(0.0, 0.0, 0.0, m);  // inside a wall: the surface is held still
    }

    // Neighbours: sides and corners of height now, sides of the motion (now - before).
    var sides = 0.0;
    var corners = 0.0;
    var vsides = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            if x != 0 || y != 0 {
                let t = textureLoad(in0, edge(p + vec2i(x, y)), 0);
                if x == 0 || y == 0 {
                    sides += t.r;
                    vsides += t.r - t.g;
                } else {
                    corners += t.r;
                }
            }
        }
    }
    let u = here.r;
    let vel = here.r - here.g;
    // Written as differences, so a level surface gives exactly 0: with the coefficients summed
    // in f32 (4 * 2/3 + 4 * 1/6 - 10/3 is not 0 there) the level would grow exponentially.
    let lap = (2.0 / 3.0) * (sides - 4.0 * u) + (1.0 / 6.0) * (corners - 4.0 * u);
    let lapv = vsides - 4.0 * vel;
    let c = clamp(params.wave_speed, 0.0, 0.85);
    var un = u + vel * (1.0 - params.damping) + c * c * lap + params.soften * lapv;
    var uc = u;

    // Absorbing edges: both time levels fade over a band with a quadratic ramp (a step in
    // damping would reflect). With the plane source the left edge stays hard: the source is there.
    if params.edges == edges_absorb {
        let dl = select(pf.x, 1e9, params.source == source_plane);
        let d = min(min(dl, f32(n.x - 1) - pf.x), min(pf.y, f32(n.y - 1) - pf.y));
        let sd = clamp(d / max(params.sponge, 1.0), 0.0, 1.0);
        let damp = 1.0 - 0.05 * (1.0 - sd) * (1.0 - sd);
        un *= damp;
        uc *= damp;
    }

    let pulse0 = textureLoad(in3, vec2i(0, 0), 0);
    let pulse1 = textureLoad(in3, vec2i(1, 0), 0);
    let pulse2 = textureLoad(in3, vec2i(2, 0), 0);

    // Steady sources: the height is driven, as the ripple tank does it. The phase at the start
    // of the frame comes from pulse.wgsl, which starts it at 0 with the simulation.
    let turns = c / max(params.wavelength, 2.0);
    let drive = params.amplitude * sin(6.2831853 * fract(pulse0.r + f32(pn.iteration + 1) * turns));
    if params.source == source_plane && pf.x < 3.0 {
        un = drive;
    } else if params.source == source_point && distance(pf, params.position * vec2f(n)) <= 2.5 {
        un = drive;
    } else if params.source == source_two {
        let mid = params.position * vec2f(n);
        let off = vec2f(0.0, 0.5 * params.spacing);
        if distance(pf, mid - off) <= 2.5 || distance(pf, mid + off) <= 2.5 {
            un = drive;
        }
    }

    // Drops, once per frame (pass 0): random rain, a beat, or the drop that falls on calm water.
    // A drop moves both time levels, so it starts at rest and spreads as a ring.
    if first {
        var dent = 0.0;
        if params.rain == rain_random {
            let f = u32(max(pn_time.frame, 0.0));
            let chance = params.rate * pulse2.r / 4.0;  // up to 4 drops per frame
            for (var j = 0u; j < 4u; j++) {
                if rand1(f, j) < chance {
                    dent += drop_at(p, rand2(f, j) * vec2f(n), params.drop_size, f32(params.rings));
                }
            }
        }
        if (params.rain == rain_beat && pulse1.r > 0.5) || pulse1.g > 0.5 {
            dent += drop_at(p, pulse1.ba * vec2f(n), params.drop_size, f32(params.rings));
        }
        un -= params.drop_strength * dent;
        uc -= params.drop_strength * dent;
    }

    // People.
    if params.push == push_press {
        un = mix(un, -0.3 * params.push_strength * m, m);
    } else if params.push == push_move {
        un -= 0.02 * params.push_strength * (m - here.a);
    }

    let acc = here.b + params.average * (u * u - here.b);
    return vec4f(un, uc, acc, m);
}
