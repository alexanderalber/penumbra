// The picture of the growth state (see step.wgsl for the channels), scaled up to this node's
// width and height. Dark background, for projection.
//
// in0: the state. in1: the object mask (or a photo), seen dimly through the ice.
// in2: the heat picture, drawn as a faint warm glow. Both may have any size.
//
// @param model choice(frost|branch|lichen) = frost -- follows the component's model
// @param now float = 0 0..100000 -- seconds since the start; bound to the component's clock
// @param mode choice(lit|state) = lit -- state: the raw channels
// @param gain float = 1 0..3 -- overall brightness
// @param background color = [0.003, 0.005, 0.012, 1] -- behind everything
// @param through float = 0.25 0..1 -- how much of in1 shows through; 0 for projection onto the object
// @param warm float = 0.25 0..1 -- how visible the heat in in2 is, as a faint glow; 0 for projection
// @param colors palette = 0b2a4a 3a7bd5 9fd8ff ffffff -- branch and lichen colours: branch: young to old is end to start; lichen: one colour per colony hue; empty: built-in colours
// @group frost
// @param tint color = [0.45, 0.7, 1.0, 1] -- colour of the ice
// @param rim float = 1 0..3 -- the bright edges of the crystals
// @param sparkle float = 1 0..3 -- glints on the ice
// @param spines float = 1 0..3 -- brightness of the branch lines inside the ice
// @param spine_width float = 2.5 1..6 -- how wide the branch lines are, in simulation texels
// @param relief float = 6 0..20 -- how steep the ice looks to the light
// @param light float = 2.2 0..6.2832 -- direction the light comes from, in radians
// @param thicken float = 60 1..200 -- seconds until ice reaches full thickness; older ice looks whiter
// @group branch and lichen
// @param fade float = 40 1..250 -- branch: seconds over which new growth fades to old colour
// @param rings float = 2.5 0..60 -- lichen: seconds per growth ring (0: none)
// @param food float = 0.05 0..1 -- branch: how visible the food field is

var<private> n: vec2f;

fn tick() -> f32 {
    return floor(params.now * 7.5);
}

fn age(birth: f32) -> f32 {
    return max(tick() - birth, 0.0);
}

fn st(uv: vec2f) -> vec4f {
    return textureSampleLevel(in0, pn_clamp, uv, 0.0);
}

fn nearest(uv: vec2f) -> vec4f {
    let q = clamp(vec2i(uv * n), vec2i(0), vec2i(n) - vec2i(1));
    return textureLoad(in0, q, 0);
}

fn lin(c: vec3f) -> vec3f {
    return pow(c, vec3f(2.2));
}

fn hash1(p: vec2u, k: u32) -> f32 {
    return f32(pn_hash(vec3u(p, k)).x >> 8u) / 16777216.0;
}

// Thickness 0..1 from the age of the ice (the tick it froze is in b).
fn thick(b: f32) -> f32 {
    return sqrt(clamp(age(b) / (params.thicken * 7.5), 0.0, 1.0));
}

fn frost_height(uv: vec2f) -> f32 {
    return st(uv).r * (0.35 + 0.65 * thick(nearest(uv).b));
}

fn frost_pic(uv: vec2f, pix: vec2f, bg: vec3f) -> vec3f {
    let p = st(uv).r;
    let o = 1.0 / n;
    let hx = frost_height(uv + vec2f(o.x, 0.0)) - frost_height(uv - vec2f(o.x, 0.0));
    let hy = frost_height(uv - vec2f(0.0, o.y)) - frost_height(uv + vec2f(0.0, o.y));
    let nrm = normalize(vec3f(-hx * params.relief, -hy * params.relief, 1.0));
    // Each crystal leans its facets its own way, so neighbouring crystals catch the light
    // differently.
    let nb = nearest(uv);
    let orient = nb.a - 1.0;
    let body = smoothstep(0.35, 0.65, p);
    let th = thick(nb.b) * body;
    let tilt = vec3f(cos(orient * 6.2831853), sin(orient * 6.2831853), 0.0) * 0.3 * body;
    let fac = normalize(nrm + tilt);
    let la = params.light + 0.25 * sin(params.now * 0.21);
    let l = normalize(vec3f(cos(la), sin(la), 0.9));
    let hv = normalize(l + vec3f(0.0, 0.0, 1.0));
    let diff = max(dot(fac, l), 0.0);
    let spec = pow(max(dot(fac, hv), 0.0), 60.0);
    // The front of the ice: a thin bright line where the phase passes 0.5.
    let front = exp(-pow((p - 0.5) / 0.14, 2.0));

    // Spines: places that froze earlier than the ice around them. They are the paths of the
    // crystal tips, so they draw the branches even after the gaps have frozen over.
    var around = 0.0;
    var cnt = 0.0;
    let a0 = age(st(uv).b);
    for (var i = 0; i < 8; i++) {
        let an = f32(i) * 0.785398;
        let t = st(uv + vec2f(cos(an), sin(an)) * params.spine_width / n);
        if t.r > 0.5 {
            around += age(t.b);
            cnt += 1.0;
        }
    }
    var spine = 0.0;
    if cnt > 0.0 {
        spine = clamp((a0 - around / cnt) / 6.0, 0.0, 1.0) * body;
    }
    let tint = params.tint.rgb;
    // Clear ice lets the background through, dimmed; older ice is whiter and more opaque.
    var col = bg * (1.0 - 0.3 * body);
    col += tint * body * (0.04 + 0.16 * th) * (0.35 + 1.0 * diff);
    col += mix(tint, vec3f(1.0), 0.5) * spine * 0.6 * params.spines;
    col += mix(tint, vec3f(1.0), 0.6) * front * 0.5 * params.rim;
    col += vec3f(0.75, 0.88, 1.0) * spec * body * (0.05 + 0.25 * th) * params.sparkle;
    // Glints: a few display pixels on older ice flash now and then.
    let cell = vec2u(pix / 2.0);
    let gh = hash1(cell, 31u);
    let tw = 0.5 + 0.5 * sin(params.now * (0.7 + 2.5 * hash1(cell, 32u)) + gh * 400.0);
    let glint = select(0.0, pow(tw, 30.0), gh < 0.006) * th * (0.3 + diff);
    col += vec3f(0.9, 0.95, 1.0) * glint * 2.5 * params.sparkle;
    return col;
}

fn pal_count() -> i32 {
    return i32(params.colors.count);
}

fn pal_map(t: f32) -> vec3f {
    return pn_colormap(params.colors, t).rgb;
}

fn pal_band(t: f32) -> vec3f {
    return pn_band(params.colors, t).rgb;
}

fn branch_pic(uv: vec2f, bg: vec3f) -> vec3f {
    let s = st(uv);
    let a = nearest(uv);
    // A soft glow: eight taps around.
    var glow = 0.0;
    for (var i = 0; i < 8; i++) {
        let an = f32(i) * 0.785398;
        glow += st(uv + vec2f(cos(an), sin(an)) * 3.0 / n).r;
    }
    glow = glow / 8.0;
    var t = 1.0;
    if a.r > 0.5 {
        t = clamp(age(a.b) / (params.fade * 7.5), 0.0, 1.0);
    }
    var tc = mix(lin(vec3f(0.6, 0.85, 1.0)), lin(vec3f(0.1, 0.2, 0.45)), t);
    if pal_count() > 0 {
        tc = pal_map(1.0 - t);
    }
    var col = bg + vec3f(0.02, 0.03, 0.05) * (1.0 - s.g) * params.food * 4.0;
    col += tc * glow * 0.5 * (1.0 - 0.6 * t);
    col = mix(col, tc * (1.0 - 0.55 * t) + vec3f(0.6) * (1.0 - t) * (1.0 - t) * 0.6, s.r);
    return col;
}

fn lichen_pic(uv: vec2f, bg: vec3f) -> vec3f {
    let a = nearest(uv);
    var col = bg;
    let occ = clamp(st(uv).r * 4.0, -1.0, 1.0);
    if a.r > 0.0 {
        var base = vec3f(0.5, 0.6, 0.3);
        if pal_count() > 0 {
            base = pal_band(a.r);
        }
        let ag = age(a.b) / 7.5;
        var ring = 1.0;
        if params.rings > 0.0 {
            ring = 0.75 + 0.25 * cos(6.2831853 * (a.b / 7.5) / params.rings);
        }
        // Young rim lighter, old parts darker and greyer.
        let young = exp(-ag / 4.0);
        let old = clamp(ag / 120.0, 0.0, 1.0);
        var c = base * ring * (0.85 + 0.3 * (a.g - 0.5));
        c = mix(c, vec3f(dot(c, vec3f(0.33))), old * 0.5) * (1.0 - 0.35 * old);
        c = c + (vec3f(1.0) - c) * young * 0.35;
        // Dark seam where two colonies meet.
        let o = 1.0 / n;
        let h1 = nearest(uv + vec2f(o.x, 0.0)).r;
        let h2 = nearest(uv + vec2f(0.0, o.y)).r;
        let seam = (h1 > 0.0 && abs(h1 - a.r) > 0.001) || (h2 > 0.0 && abs(h2 - a.r) > 0.001);
        c = select(c, c * 0.35, seam);
        col = c * 0.8;
    } else if a.r < 0.0 {
        // Dead crust, fading out.
        let f = clamp(1.0 - age(a.b) / 7.5 / 40.0, 0.25, 1.0);
        col = mix(bg, lin(vec3f(0.32, 0.28, 0.24)) * (0.7 + 0.5 * a.g), 0.6 * f);
    }
    col *= 0.9 + 0.1 * occ;
    return col;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    n = vec2f(textureDimensions(in0));
    if params.mode == mode_state {
        let s = st(v.uv);
        return vec4f(s.r, s.g, s.b, 1.0);
    }
    let model = params.model;
    let through = params.through;
    let warm = params.warm;

    let behind = textureSampleLevel(in1, pn_clamp, v.uv, 0.0).rgb;
    var bg = params.background.rgb + behind * through * 0.04;
    let heat = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, v.uv, 0.0).rgb), 0.0, 1.0);
    bg += vec3f(0.08, 0.025, 0.005) * heat * warm;

    var col = bg;
    if model == model_frost {
        col = frost_pic(v.uv, v.pos.xy, bg);
    } else if model == model_branch {
        col = branch_pic(v.uv, bg);
    } else {
        col = lichen_pic(v.uv, bg);
    }
    col = col * params.gain;
    col = col / (1.0 + col * 0.15);
    return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), 1.0);
}
