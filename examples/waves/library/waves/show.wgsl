// Turns the ripple state (r = height, b = energy mean) into a picture at this node's size; the
// state is scaled up smoothly.
//
// in0: the state. in1: the walls (walls.wgsl). in2: the people input, any size.
//
// @param view choice(water|height|energy|relief) = water -- water: a lit surface over a floor that bends and gathers light; height: crests and troughs in the colours; energy: where the water moves on average (interference fringes); relief: grey shaded slopes
// @param floor choice(tiles|sand|dark) = tiles -- water view: what lies under the water
// @param gain float = 1.5 0..10 -- height view: how strongly the waves colour
// @param energy_gain float = 3 0..10 -- energy view: how strongly the motion colours
// @param colors palette = ffcf67 e09f57 c17549 a0513e 773339 572949 4c3d73 5163a2 5f89c3 6fb7e2 80e7ff -- height view: troughs to crests
// @param heat palette = magma -- energy view: still to busy
// @param marker float = 0.5 0..1 -- how visible the people input is, in marker_color
// @param wall_show float = 1 0..1 -- how visible the walls are, in wall_color
// @param cutout bool = false -- walls black and transparent (alpha 0) and no wall shadow, for projection onto the object that makes them; set marker to 0 to hide the people too
// @param wall_color color = [0.55, 0.58, 0.62, 1] -- colour of the walls
// @param marker_color color = [0.95, 0.45, 0.2, 1] -- colour of the people
// @group surface
// @param slope float = 16 0..40 -- water view: how steep the waves look: light, glints and bending scale with it
// @param bend float = 10 0..40 -- water view: how far the floor shifts under a slope, in state texels
// @param caustics float = 2.0 0..6 -- water view: bright lines where crests gather the light on the floor
// @param clear float = 0.6 0..1 -- water view: how much of the floor shows through; the rest is the deep colour
// @param deep color = [0.004, 0.035, 0.06, 1] -- water view: colour of deep water
// @param sky color = [0.5, 0.65, 0.8, 1] -- water view: colour reflected by steep slopes
// @param glint float = 1.0 0..4 -- water view: the sun's highlights
// @param sun float = 2.4 0..6.2832 -- water and relief views: direction the sun comes from, in radians
// @param tiles float = 14 2..60 -- water view: floor tiles across the picture height

var<private> walls: f32;

fn h(uv: vec2f) -> f32 {
    return textureSampleLevel(in0, pn_clamp, uv, 0.0).r;
}

fn lin(c: vec3f) -> vec3f {
    return pow(c, vec3f(2.2));
}

fn wall_at(uv: vec2f) -> f32 {
    return clamp(pn_luma(textureSampleLevel(in1, pn_clamp, uv, 0.0).rgb), 0.0, 1.0);
}

fn floor_at(uv: vec2f) -> vec3f {
    var col = vec3f(0.02);
    if params.floor == floor_tiles {
        let aspect = pn.resolution.x / pn.resolution.y;
        let q = vec2f(uv.x * aspect, uv.y) * params.tiles;
        let cell = floor(q);
        let f = fract(q);
        let hh = pn_hash(vec3u(vec2u(vec2i(cell) + vec2i(1000)), 5u));
        let vr = f32(hh.x >> 8u) / 16777216.0;
        let grout = smoothstep(0.0, 0.045, min(min(f.x, 1.0 - f.x), min(f.y, 1.0 - f.y)));
        let tile = mix(lin(vec3f(0.42, 0.68, 0.76)), lin(vec3f(0.52, 0.76, 0.82)), vr);
        col = mix(lin(vec3f(0.14, 0.24, 0.28)), tile, grout);
    } else if params.floor == floor_sand {
        let aspect = pn.resolution.x / pn.resolution.y;
        let q = vec3f(uv.x * aspect * 9.0, uv.y * 9.0, 0.0);
        let r = 0.5 + 0.25 * pn_noise(q, 3u) + 0.12 * pn_noise(q * 3.1, 4u);
        col = mix(lin(vec3f(0.55, 0.47, 0.33)), lin(vec3f(0.85, 0.77, 0.58)), clamp(r, 0.0, 1.0));
    }
    return col;
}

// Walls and people over a finished colour. With `cutout` the walls are black and transparent.
fn finish(col: vec3f, uv: vec2f, wall: f32, wc: vec3f) -> vec4f {
    let m = clamp(pn_luma(textureSampleLevel(in2, pn_clamp, uv, 0.0).rgb), 0.0, 1.0) * params.marker;
    var c = mix(col, params.marker_color.rgb, m);
    if params.cutout != 0u {
        return vec4f(c * (1.0 - wall), 1.0 - wall);
    }
    c = mix(c, wc, wall * walls);
    return vec4f(clamp(c, vec3f(0.0), vec3f(1.0)), 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    walls = select(params.wall_show, 0.0, params.cutout != 0u);
    let uv = v.uv;
    let tx = 1.0 / vec2f(textureDimensions(in0));
    let st = textureSampleLevel(in0, pn_clamp, uv, 0.0);
    let wall = wall_at(uv);
    let wc = params.wall_color.rgb;

    if params.view == view_height {
        let t = clamp(0.5 + 0.5 * st.r * params.gain, 0.0, 1.0);
        let col = pn_colormap(params.colors, t).rgb;
        return finish(col, uv, wall, wc);
    }
    if params.view == view_energy {
        // the RMS height; sqrt 2 puts a wave of amplitude 1 at the brightness of its crests
        let a = clamp(sqrt(max(st.b, 0.0)) * 1.4142 * params.energy_gain * 0.5, 0.0, 1.0);
        return finish(pn_colormap(params.heat, a).rgb, uv, wall, wc);
    }

    // Slopes and curvature, in state texels whatever the output size, taken 1.5 texels apart.
    let o = 1.5;
    let hl = h(uv - vec2f(o * tx.x, 0.0));
    let hr = h(uv + vec2f(o * tx.x, 0.0));
    let hu = h(uv - vec2f(0.0, o * tx.y));
    let hd = h(uv + vec2f(0.0, o * tx.y));
    let hc = (h(uv + 0.5 * tx) + h(uv - 0.5 * tx) + h(uv + vec2f(0.5, -0.5) * tx) + h(uv + vec2f(-0.5, 0.5) * tx)) * 0.25;
    let g = vec2f(hr - hl, hd - hu) / (2.0 * o) * params.slope;
    let curv = (hl + hr + hu + hd - 4.0 * hc) / (o * o) * params.slope;
    // screen y points down; the normal's y is flipped so the sun direction reads as on screen
    let nrm = normalize(vec3f(-g.x, g.y, 1.0));

    if params.view == view_relief {
        let l = normalize(vec3f(cos(params.sun), sin(params.sun), 0.8));
        let d = max(dot(nrm, l), 0.0);
        return finish(vec3f(0.08 + 0.75 * d), uv, wall, wc);
    }

    // Water. The floor is seen through the surface, shifted along the slope (refraction),
    // and brighter under crests that gather the light (caustics).
    let fuv = uv - g * params.bend * tx;
    var fl = floor_at(fuv);
    let gather = 1.0 / max(1.0 + params.caustics * params.bend * curv * 0.5, 0.2);
    fl *= clamp(gather, 0.0, 5.0);
    // Walls cast a soft shadow on the floor, on the side away from the sun.
    let to_sun = vec2f(cos(params.sun), -sin(params.sun));
    var shade = 0.0;
    for (var i = 1; i <= 6; i++) {
        shade += wall_at(fuv + to_sun * f32(i) * 2.5 * tx);
    }
    fl *= 1.0 - 0.1 * shade * walls;
    var col = mix(params.deep.rgb, fl, params.clear);
    // Reflection: a sky that is brighter towards the sun, so slopes facing it light up and
    // slopes turned away darken. That is what makes the ripples read as a surface.
    let l = normalize(vec3f(cos(params.sun), sin(params.sun), 2.2));
    let tilt = dot(nrm.xy, normalize(l.xy)) / max(nrm.z, 0.2);
    let fres = 0.08 + 0.92 * pow(1.0 - clamp(nrm.z, 0.0, 1.0), 5.0);
    col = col * (1.0 - fres) + params.sky.rgb * fres * clamp(1.0 + 4.0 * tilt, 0.0, 2.5);
    // Sun glints: the sun stands high in direction `sun`; slopes that mirror it to the eye shine.
    let hv = normalize(l + vec3f(0.0, 0.0, 1.0));
    let spec = pow(max(dot(nrm, hv), 0.0), 400.0) * params.glint * 3.0;
    col += vec3f(spec);
    // Walls: stone, its edges lit on the sun's side and shaded on the other.
    let wl = wall_at(uv + to_sun * 2.0 * tx);
    let stone = wc * (0.9 + 0.5 * (wl - wall));
    col = col / (1.0 + col * 0.15);
    return finish(col, uv, wall, stone);
}
