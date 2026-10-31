// A person walking to and fro in front of the wall, as a depth camera's mask shows them: white
// where the person is, black everywhere else. It stands in for a Kinect, so the example runs
// without one. The arms swing with the stride and are flung up now and then.
// @param pace float = 1 0..3 -- how fast the person walks
// @param size float = 0.85 0.3..1 -- height of the person, in image heights
// @param ragged float = 0.3 0..1 -- frayed edges, as a depth camera's mask has them
// @param speckle float = 0.01 0..1 -- stray pixels near the floor, as depth cameras show them
// @param block float = 3 1..12 -- size of one depth camera pixel, in pixels of this picture: a Kinect's 512 pixels across a wider picture give steps along the outline

// Distance from p to the segment a-b, minus the radius r: below 0 inside the limb.
fn capsule(p: vec2f, a: vec2f, b: vec2f, r: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
    return length(pa - ba * h) - r;
}

// A union with a rounded seam, so limbs grow out of the body instead of meeting at a corner.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

// A direction at angle `a` from straight down, turned towards the front by `facing`.
fn limb(a: f32, facing: f32) -> vec2f {
    return vec2f(sin(a) * facing, -cos(a));
}

fn hash01(p: vec3u) -> f32 {
    return f32(pn_hash(p).x) / 4294967295.0;
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let aspect = pn.resolution.x / pn.resolution.y;
    // The depth camera's pixel this one falls in, in image heights, y up from the floor.
    let b = max(params.block, 1.0);
    let cam = (floor(v.pos.xy / b) + 0.5) * b / pn.resolution;
    let p = vec2f(cam.x * aspect, 1.0 - cam.y);
    let t = pn_time.seconds * params.pace;
    let s = params.size;

    // To and fro across the wall: x follows a sine, the stride follows the distance walked,
    // so the feet do not slide.
    let reach = aspect * 0.5 - 0.25 * s;
    let u = t * 0.16;
    let x = aspect * 0.5 + reach * sin(u);
    let k = floor((u + 1.5707963) / 3.1415927);
    let turns = select(1.0, -1.0, (i32(k) & 1) == 1);
    let walked = reach * (2.0 * k + 1.0 + turns * sin(u));
    let facing = select(-1.0, 1.0, cos(u) >= 0.0);
    let stride = walked / (0.55 * s) * 6.2831853;
    // Smaller steps near the turns, where the walk slows down.
    let swing = 0.45 * clamp(abs(cos(u)) * 1.6, 0.25, 1.0);

    let floor_y = 0.03;
    let thigh = 0.245 * s;
    let shin = 0.245 * s;
    let hip = vec2f(x, floor_y + thigh + shin - 0.01 * s + 0.012 * s * abs(sin(stride)));
    let neck = hip + vec2f(0.03 * s * facing, 0.31 * s);
    let head = neck + vec2f(0.015 * s * facing, 0.085 * s);

    var d = capsule(p, hip, neck - vec2f(0.0, 0.03 * s), 0.075 * s);
    // A coat, wider towards the hem.
    let hem = clamp((hip.y + 0.06 * s - p.y) / (0.2 * s), 0.0, 1.0);
    let coat = capsule(p, hip + vec2f(0.0, 0.05 * s), hip - vec2f(0.0, 0.12 * s), (0.08 + 0.04 * hem) * s);
    d = smin(d, coat, 0.03 * s);
    d = smin(d, length(p - head) - 0.065 * s, 0.03 * s);

    for (var i = 0; i < 2; i++) {
        let side = select(0.0, 3.1415927, i == 1);
        let a = swing * sin(stride + side);
        // The knee bends while the leg swings forward.
        let bend = 0.7 * max(0.0, sin(stride + side + 1.2));
        let knee = hip + limb(a, facing) * thigh;
        let ankle = knee + limb(a - bend, facing) * shin;
        let toe = ankle + vec2f(0.07 * s * facing, 0.005 * s);
        d = smin(d, capsule(p, hip, knee, 0.048 * s), 0.02 * s);
        d = min(d, capsule(p, knee, ankle, 0.036 * s));
        d = smin(d, capsule(p, ankle, toe, 0.022 * s), 0.01 * s);

        // The arms swing against the legs. Now and then one is flung up, the second one
        // backwards.
        let fi = f32(i);
        let fling = smoothstep(0.55, 0.9, sin(t * (0.41 + 0.13 * fi) + fi * 2.3));
        let way = select(facing, -facing, i == 1 && fling > 0.01);
        let shoulder = neck + vec2f(0.0, -0.035 * s);
        let up = -a * 0.8 + fling * (1.9 + 0.3 * sin(t * 3.0 + fi));
        let elbow = shoulder + limb(up, way) * 0.16 * s;
        let hand = elbow + limb(up * 1.15 + 0.35, way) * 0.15 * s;
        d = smin(d, capsule(p, shoulder, elbow, 0.03 * s), 0.02 * s);
        d = min(d, capsule(p, elbow, hand, 0.025 * s));
    }

    // Frayed edges: fine, quick noise added to the distance, as a depth sensor flickers.
    d += params.ragged * 0.012 * pn_noise(vec3f(p * 90.0, t * 6.0), 7u);

    let px = 0.5 * b / pn.resolution.y;
    var mask = 1.0 - smoothstep(-px, px, d);

    // Stray pixels near the floor, a new set every frame.
    let cell = vec3u(vec2u(v.pos.xy / 3.0), u32(pn_time.frame));
    let near_floor = 1.0 - smoothstep(0.0, 0.12, p.y);
    if hash01(cell) < params.speckle * 0.02 * near_floor {
        mask = 1.0;
    }
    return vec4f(vec3f(mask), 1.0);
}
