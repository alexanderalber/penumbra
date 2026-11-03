// A planet with a tilted ring over black, standing over the horizon. Bands of cloud in the
// stage's palette run around it and drift as it turns. Over a track it grows and rises: small
// at the track's start, at its largest at the track's end. The ring's
// near half passes in front of the disc, its far half behind it. Nothing below the horizon.
// In a tube (`bend` 1) it hangs in the middle of the tube's far end, small enough to fit in.
// Black where the sky is empty, so it can be screened over another sky.
// @param grow float = 0 0..1 -- how far through its track the music is (track_phase of a chan.beat_grid): the planet grows and rises with it
// @param size float = 0.09 0.02..0.5 -- the planet's radius at a track's start, in picture heights
// @param growth float = 1 0..4 -- how much larger it is at a track's end: 1 twice the size
// @param place float = -0.42 -1.5..1.5 -- where it stands across the sky, in picture heights from straight ahead
// @param above float = 0.16 -0.2..1 -- its centre over the horizon at a track's start, in picture heights
// @param rise float = 0.1 0..1 -- how much higher it stands at a track's end
// @param tilt float = 18 -90..90 -- the ring's tilt, degrees
// @param open float = 0.3 0.05..1 -- how far the ring opens towards the viewer: 0 edge on, 1 a circle
// @param bands float = 7 1..30 -- cloud bands from pole to pole
// @param spin float = 0.01 0..0.5 -- turns per beat travelled
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param energy float = 0 0..1 -- the bass (energy_low of a chan.beat_grid): the planet swells and brightens with it
// @param breathe float = 0 0..0.5 -- how much the bass swells it; 0 keeps it still
// @param warmth float = 0 0..1 -- 0 the warm palette, 1 the cold one, as the stage's
// @param brightness float = 1 0..3 -- the whole picture's light
// @param presence float = 1 0..1 -- how much of the planet is there: 1 all of it, 0 nothing (a dark sky)
// @param eye float = 1.5 0..10 -- the camera's height over the floor, metres (bind it to height of the stage's pose)
// @param pitch float = -2 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 60 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 10 1..100 -- metres ahead where the floor fades out, the stage's `end`: the horizon lies there
// @param bend float = 0 0..1 -- 0 it stands over the horizon; 1 it hangs in a tube's far end, inside its ring (curve of the stage's pose)
// @param axis float = 4 0..20 -- the tube's axis, metres over the floor (axis of the stage's pose)
// @param radius float = 4 0..20 -- the tube's radius, metres (radius of the stage's pose)
// @param far float = 20 1..100 -- metres ahead where the tube fades out: its far end lies there (far of the stage's pose)

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. The horizon is where the floor fades out, `reach` metres ahead, as the
// camera sees it: it moves with the camera's height, pitch, roll and turn, and things in the sky
// keep their size in degrees when the field of view changes. `.z`: one pixel in the same unit.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    // Tangents of the angle from the view's middle, y up.
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan(down)) / unit, 2.0 * t / (res.y * unit));
}

// The tube's far end as the camera sees it, in sky_at's units: `.xy` its middle, `.z` its radius.
// The end is the ring of `radius` round the tube's axis, `axis` metres up and `far` metres ahead.
// The camera sits below the axis, so the middle lies above the vanishing point.
fn tube_end() -> vec3f {
    let unit = 2.0 * tan(radians(30.0));
    let pitch = radians(params.pitch);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + pitch;
    let far = max(params.far, 0.5);
    let r = max(params.radius, 0.0);
    let top = tan(atan((params.axis + r - params.eye) / far) - pitch);
    let bottom = tan(atan((params.axis - r - params.eye) / far) - pitch);
    return vec3f(0.0, (0.5 * (top + bottom) + tan(down)) / unit, max(0.5 * (top - bottom) / unit, 0.02));
}

// The palette: warm (0) runs yellow, orange, magenta; cold (1) cyan, blue, violet. `s` 0 to 1.
fn palette(s: f32) -> vec3f {
    let warm = mix(mix(vec3f(1.0, 0.86, 0.3), vec3f(1.0, 0.42, 0.18), smoothstep(0.0, 0.5, s)), vec3f(0.95, 0.12, 0.55), smoothstep(0.45, 1.0, s));
    let cold = mix(mix(vec3f(0.35, 0.95, 1.0), vec3f(0.25, 0.45, 1.0), smoothstep(0.0, 0.5, s)), vec3f(0.62, 0.18, 1.0), smoothstep(0.45, 1.0, s));
    return mix(warm, cold, clamp(params.warmth, 0.0, 1.0));
}

fn hash(a: u32, b: u32, c: u32) -> f32 {
    return f32(pn_hash(vec3u(a, b, c)).x) / 4294967296.0;
}

fn rot(p: vec2f, a: f32) -> vec2f {
    return vec2f(cos(a) * p.x - sin(a) * p.y, sin(a) * p.x + cos(a) * p.y);
}

// The ring's light at `rho` planet radii from the centre: three rings with gaps between them.
fn ring_light(rho: f32, aa: f32) -> f32 {
    let inner = smoothstep(1.35 - aa, 1.35 + aa, rho) * (1.0 - smoothstep(1.62 - aa, 1.62 + aa, rho));
    let outer = smoothstep(1.7 - aa, 1.7 + aa, rho) * (1.0 - smoothstep(2.05 - aa, 2.05 + aa, rho));
    let faint = smoothstep(2.12 - aa, 2.12 + aa, rho) * (1.0 - smoothstep(2.3 - aa, 2.3 + aa, rho));
    let grain = 0.75 + 0.25 * sin(rho * 60.0);
    return (0.8 * inner + outer + 0.35 * faint) * grain;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let s = at.xy;
    let grow = clamp(params.grow, 0.0, 1.0);
    let energy = clamp(params.energy, 0.0, 1.0);
    let swell = 1.0 + params.breathe * energy;
    // In the tube the planet and its ring fit inside the far end: the ring's outer edge reaches
    // at most 0.8 of the end's radius when the planet is at its largest.
    let b = clamp(params.bend, 0.0, 1.0);
    let end = tube_end();
    let sky_r = max(params.size, 0.005) * (1.0 + params.growth * grow);
    let tube_r = end.z * 0.26 * (1.0 + 0.5 * grow);
    let r = mix(sky_r, tube_r, b) * swell;
    let centre = mix(vec2f(params.place, params.above + params.rise * grow), end.xy, b);
    let u = rot(s - centre, -radians(params.tilt)) / r;
    let aa = 1.5 * at.z / r;
    let d = length(u);
    let disc = 1.0 - smoothstep(1.0 - aa, 1.0 + aa, d);

    // The sphere: cloud bands by latitude, bent by noise that drifts with the turn.
    let z = sqrt(max(1.0 - d * d, 0.0));
    let lat = asin(clamp(u.y, -1.0, 1.0));
    let lon = atan2(u.x, max(z, 1e-4)) + 6.2831853 * params.spin * params.beats;
    let n = pn_noise(vec3f(lon * 1.5, lat * 6.0, 0.3), 11u) + 0.5 * pn_noise(vec3f(lon * 4.0, lat * 14.0, 1.7), 12u);
    let band = 0.5 + 0.5 * sin(lat * params.bands + 1.3 * n);
    let tone = fract(0.5 * lat / 1.5708 + 0.5 + 0.18 * n);
    let light_dir = normalize(vec3f(-0.55, 0.45, 0.7));
    let shade = clamp(dot(vec3f(u, z), light_dir), 0.0, 1.0);
    let rim = pow(1.0 - z, 3.0);
    var body = palette(tone) * (0.25 + 0.75 * band) * (0.12 + 0.88 * shade) + palette(0.9) * rim * 0.6;
    body *= disc * (0.85 + 0.3 * energy);

    // The ring, an ellipse in the planet's frame; its near half lies below the centre.
    let rho = length(vec2f(u.x, u.y / max(params.open, 0.05)));
    let ring_aa = aa / max(params.open, 0.05);
    var ring = ring_light(rho, ring_aa);
    // The planet's shadow falls on the ring's far side, behind the planet.
    let behind = u.y > 0.0;
    if behind {
        ring *= 1.0 - disc;
    }
    let ring_colour = palette(0.15 + 0.5 * smoothstep(1.3, 2.3, rho)) * 0.8;
    var light = body + ring_colour * ring;
    if !behind {
        light = body * (1.0 - 0.85 * min(ring, 1.0)) + ring_colour * ring;
    }

    // A faint glow around the disc.
    let halo = 0.18 * exp(-max(d - 1.0, 0.0) * 5.0) * (1.0 - disc);
    light += palette(0.6) * halo * (0.7 + 0.5 * energy);

    // Nothing below the horizon, and in the tube nothing outside its far end.
    let horizon = smoothstep(-at.z, at.z, s.y);
    let inside = 1.0 - smoothstep(end.z - at.z, end.z + at.z, length(s - end.xy));
    light *= mix(horizon, inside, b);
    return vec4f(light * params.brightness * clamp(params.presence, 0.0, 1.0), 1.0);
}
