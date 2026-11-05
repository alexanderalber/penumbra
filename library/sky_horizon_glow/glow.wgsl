// A second layer of the first light, over black, to screen over the band of `stage_valley`: broad
// soft rays fanning up from below the horizon and a faint halo above the band. Both breathe with
// the stage's swell, so they are strongest in a breakdown and sink back on the drop. The dawn
// widens them and makes them warmer. There is no disc: the light comes from below the horizon.
// The rays turn slowly as the world travels. Nothing below the horizon. The picture is light with
// alpha 0: laid `over` other sky elements it adds to them and hides nothing.
// @param presence float = 1 0..1 -- how much of the glow is there: 1 all of it, 0 nothing
// @param brightness float = 1 0..3 -- the light of rays and halo together
// @param rays float = 1 0..3 -- the rays' light; 0 only the halo
// @param count float = 7 1..30 -- about how many rays fan over the band
// @param tall float = 0.45 0.05..1.5 -- how high the rays reach at swell 1 and dawn 1, in picture heights
// @param halo float = 1 0..3 -- the halo's light; 0 only the rays
// @param calm float = 0.35 0..1 -- the light left when the swell is 0 (in a full beat): 0 none, 1 as in a breakdown
// @param warmth float = 0 0..1 -- 0 the ember palette (copper rays, amber halo), 1 the jade palette (pale bronze rays, jade halo)
// @param seed int = 0 0..1000 -- picks another set of rays
// @param turn float = 0.0015 0..0.02 -- how fast the rays wander, per beat travelled
// @param dawn float = 0 0..1 -- the stage's dawn (bind it to dawn of the stage's pose): wider, warmer light towards 1
// @param swell float = 0 0..1 -- the stage's swell (swell of the stage's pose): the glow's breath
// @param beats float = 0 0..20000 -- distance travelled, in beats (travelled of a chan.beat_grid)
// @param eye float = 1.2 0..10 -- the camera's height over the floor, metres (height of the stage's pose)
// @param pitch float = -1 -60..60 -- the camera's tilt up, degrees (pitch of the stage's pose)
// @param fov float = 58 10..120 -- the camera's vertical field of view, degrees (fov of the stage's pose)
// @param roll float = 0 -45..45 -- the camera's roll, degrees (roll of the stage's pose)
// @param side float = 0 -45..45 -- the camera's turn to the right, degrees (side of the stage's pose)
// @param reach float = 40 1..200 -- metres ahead where the floor fades out, the stage's `end` (far of the pose): the horizon lies there

// Where this pixel lies in the sky, in picture heights of a 60 degree view: x from straight ahead,
// y up from the horizon. `.z`: one pixel in the same unit. The same helper as the stage's sky.
fn sky_at(px: vec2f) -> vec3f {
    let res = pn.resolution;
    let t = tan(radians(0.5 * clamp(params.fov, 1.0, 170.0)));
    let q = vec2f((px.x - 0.5 * res.x) / res.y, 0.5 - px.y / res.y) * 2.0 * t;
    let r = radians(-params.roll);
    let p = vec2f(cos(r) * q.x - sin(r) * q.y, sin(r) * q.x + cos(r) * q.y);
    let down = atan(max(params.eye, 0.0) / max(params.reach, 0.1)) + radians(params.pitch);
    let unit = 2.0 * tan(radians(30.0));
    return vec3f(vec2f(p.x + tan(radians(params.side)), p.y + tan(down)) / unit, 2.0 * t / (res.y * unit));
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let at = sky_at(v.pos.xy);
    let x = at.x;
    let y = at.y;
    let px = max(at.z, 1e-5);
    if y < -2.0 * px {
        return vec4f(0.0);
    }
    let dawn = clamp(params.dawn, 0.0, 1.0);
    let swell = clamp(params.swell, 0.0, 1.0);
    let jade = clamp(params.warmth, 0.0, 1.0);
    let s = f32(max(params.seed, 0));
    let up = max(y, 0.0);

    // How much light there is: the swell breathes it, the dawn carries it.
    let breath = mix(clamp(params.calm, 0.0, 1.0), 1.0, smoothstep(0.0, 1.0, swell));
    let level = breath * mix(0.35, 1.0, dawn);

    // The rays come from a point below the horizon, so no disc shows.
    let depth = 0.3;
    let d = vec2f(x, up + depth);
    let angle = atan2(d.x, d.y);
    let dist = length(d) - depth;
    let n = max(params.count, 1.0);
    let wander = params.beats * params.turn;
    let a = pn_noise(vec3f(angle * n * 0.6, wander, s * 0.71), 31u)
          + 0.5 * pn_noise(vec3f(angle * n * 1.3, wander * 1.7, s * 0.37 + 5.0), 37u);
    let ray = pow(clamp(0.55 + 0.5 * a, 0.0, 1.0), 2.0);
    let len = max(params.tall, 0.01) * mix(0.45, 1.0, dawn) * mix(0.55, 1.0, swell);
    // Rays fade along their length and towards the sides of the fan.
    let fan = pow(max(cos(angle), 0.0), 3.0);
    let along = exp(-max(dist, 0.0) / len) * smoothstep(0.0, 0.03, up + 0.01);
    let ray_c = mix(vec3f(0.30, 0.17, 0.050), vec3f(0.22, 0.18, 0.095), jade) * mix(0.6, 1.0, dawn);
    let rays = ray_c * ray * fan * along * max(params.rays, 0.0) * 0.9;

    // The halo: a low wide dome over the band's middle.
    let hw = 0.75 * mix(0.6, 1.0, dawn) * (1.0 + 0.3 * swell);
    let hh = 0.10 * mix(0.6, 1.0, dawn) * (1.0 + 0.6 * swell);
    let dome = exp(-(x * x) / (hw * hw)) * exp(-up / hh);
    let halo_c = mix(vec3f(0.30, 0.15, 0.035), vec3f(0.035, 0.15, 0.095), jade);
    let halo = halo_c * dome * max(params.halo, 0.0);

    let edge = smoothstep(-px, px, y);
    let out = (rays + halo) * level * max(params.brightness, 0.0) * clamp(params.presence, 0.0, 1.0) * edge;
    return vec4f(out, 0.0);
}
