// A lamp that is not there, going round an object. A scan's height is the height over the
// surface the object stands on, not its depth towards the camera, so it cannot shade a body seen
// from the front (lit from its height alone a round object comes out as a tilted plane). Instead
// the body is taken as round: the normal from the distance to the silhouette (a dome of
// `radius`), the ribs from the camera's photo (each furrow a dark line in it, see the furrows
// effect).
// in0: a labels picture (green the object), so only the objects are lit; in1: the distance from
// the silhouette (scan_labels' `sdf`, softened), in2: the photo on the object lightly softened,
// in3: the same blurred wide (photo_detail's `fine` and `wide`).
// @param radius float = 200 20..600 -- how far in from the silhouette the body faces the camera, pixels (about half the object's width)
// @param ribs float = 3 0..10 -- how much darker the furrows are drawn, per share they are darker in the photo
// @param speed float = 0.1 -1..1 -- turns of the lamp per second
// @param elevation float = 35 0..90 -- the lamp's angle over the picture plane, degrees
// @param ambient float = 0.05 0..1
// @param color color = [1, 0.42, 0.08, 1]
// @param rim float = 0.4 0..2 -- light on the slopes facing away, as from a second, dim lamp
// @param clock float = -1 -1..600 -- seconds since the effect started (bind a chan.clock); below 0: the show time

// A label of /clean: its red is the face, its green the object, k / 255.
fn label(t: texture_2d<f32>, p: vec2i, channel: i32) -> i32 {
    let size = vec2i(textureDimensions(t));
    let c = textureLoad(t, clamp(p, vec2i(0), size - 1), 0);
    return i32(round(select(c.r, c.g, channel == 1) * 255.0));
}

fn grey(c: vec4f) -> f32 {
    return dot(c.rgb, vec3f(0.2126, 0.7152, 0.0722)) / max(c.a, 1e-4);
}

// The distance inside the silhouette at a point of the picture (0 to 1), pixels.
fn inside(uv: vec2f) -> f32 {
    return -textureSampleLevel(in1, pn_clamp, uv, 0.0).r * f32(textureDimensions(in1).x);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = vec2i(v.pos.xy);
    if (label(in0, p, 1) == 0) {
        return vec4f(0.0);
    }
    let size = vec2f(textureDimensions(in1));
    let step = 3.0 / size;
    let t = max(inside(v.uv), 0.0);
    // Outwards in the picture: down the distance's slope (x right, y down).
    let gx = inside(v.uv + vec2f(step.x, 0.0)) - inside(v.uv - vec2f(step.x, 0.0));
    let gy = inside(v.uv + vec2f(0.0, step.y)) - inside(v.uv - vec2f(0.0, step.y));
    let out = -normalize(vec2f(gx, gy) + vec2f(1e-6, 0.0));
    // A dome: r the distance from the middle in units of the radius.
    let r = clamp(1.0 - t / params.radius, 0.0, 1.0);
    let n = normalize(vec3f(out * r, sqrt(max(1.0 - r * r, 0.0)) + 0.05));
    let az = select(pn_time.seconds, params.clock, params.clock >= 0.0) * params.speed * 6.2831853;
    let el = radians(params.elevation);
    // Picture axes: x right, y down, z out of the picture towards the camera.
    let l = vec3f(cos(az) * cos(el), sin(az) * cos(el), sin(el));
    let lambert = max(dot(n, l), 0.0);
    let back = max(dot(n, vec3f(-l.xy, l.z)), 0.0) * params.rim * 0.3;
    // The ribs: darker where the photo is darker than around it.
    let fine = textureLoad(in2, p, 0);
    let wide = textureLoad(in3, p, 0);
    var ribs = 1.0;
    if (fine.a > 0.5 && wide.a > 0.5) {
        let dark = (grey(wide) - grey(fine)) / max(grey(wide), 1e-3);
        ribs = clamp(1.0 - params.ribs * dark, 0.2, 1.3);
    }
    let light = (params.ambient + 0.9 * lambert * lambert + back) * ribs;
    return vec4f(params.color.rgb * light, 1.0);
}
