// Stand-in for the plate's outline, outside the component: white on the plate, black off it.
// Later a scan mask of a real object takes its place in the component's input `shape`.
// in0: a picture whose bright part is the plate (here a letter), for shape picture.
// @param shape choice(guitar|star|square|circle|picture) = guitar -- the plate's outline
// @param size float = 0.9 0.2..1 -- height of the shape in picture heights
// @param points int = 5 3..12 -- star: number of points
// @param depth float = 0.45 0.1..0.9 -- star: how deep the notches between the points go

const PI = 3.14159265;

fn star(c: vec2f, k: f32, depth: f32) -> bool {
    let r = length(c);
    let a = atan2(c.x, -c.y);
    let seg = 2.0 * PI / k;
    let t = abs(a - seg * floor(a / seg + 0.5)) / (0.5 * seg);
    // Straight edges from a point (radius 1) to a notch (radius 1 - depth).
    let rn = 1.0 - depth;
    let ang = t * 0.5 * seg;
    let tip = vec2f(1.0, 0.0);
    let notch = rn * vec2f(cos(0.5 * seg), sin(0.5 * seg));
    let e = notch - tip;
    let dir = vec2f(cos(ang), sin(ang));
    // Ray from the centre along dir meets the edge tip + s e.
    let den = dir.x * e.y - dir.y * e.x;
    let s = -(dir.x * tip.y - dir.y * tip.x) / den;
    let edge = tip + s * e;
    return r <= length(edge);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    let c = (v.uv - 0.5) * vec2f(aspect, 1.0) / (0.5 * params.size);
    var on = false;
    if params.shape == shape_guitar {
        // Two round bouts and a waist, as the top of a guitar seen from the front.
        let lower = length((c - vec2f(0.0, 0.38)) / vec2f(0.62, 0.62)) <= 1.0;
        let upper = length((c - vec2f(0.0, -0.45)) / vec2f(0.47, 0.5)) <= 1.0;
        let waist = abs(c.x) < 0.42 && c.y > -0.45 && c.y < 0.38;
        on = lower || upper || waist;
    } else if params.shape == shape_star {
        on = star(c, f32(params.points), params.depth);
    } else if params.shape == shape_square {
        on = all(abs(c) <= vec2f(1.0));
    } else if params.shape == shape_circle {
        on = length(c) <= 1.0;
    } else {
        on = pn_luma(textureSampleLevel(in0, pn_clamp, v.uv, 0.0).rgb) > 0.5;
    }
    let w = select(0.0, 1.0, on);
    return vec4f(w, w, w, 1.0);
}
