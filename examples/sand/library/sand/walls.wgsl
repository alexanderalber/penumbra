// The wall picture for the sand, at the simulation size: white where grains cannot go. It
// holds a built-in shape and, with object on, the component's wall input (a scanned object
// mask, or a text).
//
// in0: the wall input. Brighter than level is wall, while object is on.
//
// @param shape choice(none|hourglass|cups|ramps|pegs|bar) = none -- a built-in shape; bar: a bar that moves through the sand
// @param object bool = true -- the wall input counts as wall
// @param level float = 0.5 0..1 -- the wall input brighter than this is wall
// @param neck float = 0.022 0.005..0.1 -- hourglass: width of the neck, in picture heights
// @param slope float = 60 10..80 -- ramps: slant of the shelves, in degrees; steeper than 45 and sand runs off them, flatter and it piles up on them

fn hourglass(q: vec2f) -> bool {
    // q: centred, in picture heights. Two bulbs: a round cap over a cone towards the neck.
    let x = abs(q.x);
    let y = abs(q.y);
    let half_neck = params.neck * 0.5;
    // Cone walls 60 degrees from the horizontal.
    let cone = half_neck + y * 0.577;
    // Round cap: circle of radius 0.2 centred 0.24 from the neck.
    let dy = y - 0.24;
    let cap = sqrt(max(0.04 - dy * dy, 0.0));
    var hw = min(cone, cap);
    if y < 0.24 {
        hw = min(cone, 0.2);
    }
    let open = y < 0.44 && x < hw;
    return !open;
}

fn cups(q: vec2f) -> bool {
    var wall = false;
    for (var i = 0; i < 3; i++) {
        let c = vec2f(-0.55 + 0.55 * f32(i), 0.15 - 0.12 * f32(i % 2));
        let d = q - c;
        let r = length(d);
        // Lower half of a ring: a bowl open at the top.
        if d.y > 0.0 && r > 0.13 && r < 0.155 {
            wall = true;
        }
    }
    return wall;
}

fn ramps(q: vec2f) -> bool {
    var wall = false;
    for (var i = 0; i < 4; i++) {
        let fi = f32(i);
        let left = (i % 2) == 0;
        let c = vec2f(select(0.06, -0.06, left), -0.36 + 0.24 * fi);
        let a = radians(select(-params.slope, params.slope, left));
        let d = q - c;
        let r = vec2f(cos(a) * d.x + sin(a) * d.y, -sin(a) * d.x + cos(a) * d.y);
        if abs(r.x) < 0.17 && abs(r.y) < 0.012 {
            wall = true;
        }
    }
    return wall;
}

fn pegs(q: vec2f) -> bool {
    var wall = false;
    if q.y > -0.3 && q.y < 0.3 {
        let row = floor((q.y + 0.3) / 0.075);
        let off = select(0.0, 0.05, (i32(row) % 2) == 1);
        let cx = (floor((q.x + off) / 0.1) + 0.5) * 0.1 - off;
        let cy = -0.3 + (row + 0.5) * 0.075;
        if length(q - vec2f(cx, cy)) < 0.014 {
            wall = true;
        }
    }
    return wall;
}

// A bar of 0.5 x 0.07 picture heights that moves across and up and down through the sand.
fn bar(q: vec2f, aspect: f32) -> bool {
    let t = pn_time.seconds;
    let cx = 0.5 + 0.25 * sin(6.2831853 * 0.04 * t);
    let cy = 0.62 + 0.22 * sin(6.2831853 * (0.1 * t + 0.75));
    let c = vec2f((cx - 0.5) * aspect, cy - 0.5);
    let d = abs(q - c);
    return d.x < 0.25 && d.y < 0.035;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let q = vec2f((v.uv.x - 0.5) * aspect, v.uv.y - 0.5);
    let shape = params.shape;
    let object = params.object != 0u;
    var w = 0.0;
    if shape == shape_hourglass && hourglass(q) {
        w = 1.0;
    } else if shape == shape_cups && cups(q) {
        w = 1.0;
    } else if shape == shape_ramps && ramps(q) {
        w = 1.0;
    } else if shape == shape_pegs && pegs(q) {
        w = 1.0;
    } else if shape == shape_bar && bar(q, aspect) {
        w = 1.0;
    }
    if object {
        let t = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
        w = max(w, select(0.0, 1.0, max(t.r, max(t.g, t.b)) > params.level));
    }
    return vec4f(w, w, w, 1.0);
}
