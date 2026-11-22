// A person walking in front of the wall, as a depth camera's mask shows them: white where the
// person is, black everywhere else. It stands in for a Kinect, so the example runs without one.
// The feet stay put on the floor while they carry the weight, the knees bend forward, and the arms
// swing against the legs. Now and then one arm is flung up.
// @param pace float = 1 0..3 -- how fast the person moves
// @param size float = 0.95 0.3..1.6 -- height of the person, in image heights
// @param ragged float = 0.3 0..1 -- frayed edges, as a depth camera's mask has them
// @param speckle float = 0.01 0..1 -- stray pixels near the floor, as depth cameras show them
// @param block float = 3 1..12 -- size of one depth camera pixel, in pixels of this picture: a Kinect's 512 pixels across a wider picture give steps along the outline

// Distance from p to the segment a-b, minus the radius r: below 0 inside the limb.
fn capsule(p: vec2f, a: vec2f, b: vec2f, r: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-8), 0.0, 1.0);
    return length(pa - ba * h) - r;
}

// A union with a rounded seam, so limbs grow out of the body instead of meeting at a corner.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

// Where the person is at time t, from -1 to 1 across the wall. A quick walk to the right, a slow
// way back to the left, and a few hesitations on top. The stripes behind a slow move lie close
// together and run against those behind a quick one, so this mix gives both kinds.
fn way(t: f32) -> f32 {
    let round = 14.0;
    let u = fract(t / round);
    let run = 0.3;
    var along = 1.0 - 2.0 * (u - run) / (1.0 - run);
    if u < run {
        // The walk slows down and speeds up three times, which bends the stripes it leaves.
        let s = smoothstep(0.0, run, u);
        along = 2.0 * (s + 0.035 * sin(s * 18.85)) - 1.0;
    }
    let steps = 0.04 * sin(t * 0.83 + 1.0) * sin(t * 0.37);
    return along + steps;
}

// The knee between hip and foot, two bones of length `bone`, bent towards `front` (-1 left,
// 1 right; 0 shows the leg from the front, straight).
fn knee_of(hip: vec2f, foot: vec2f, bone: f32, front: f32) -> vec2f {
    let d = foot - hip;
    let len = max(length(d), 1e-5);
    let half = min(0.5 * len, bone);
    let out = sqrt(max(bone * bone - half * half, 0.0));
    // Square to the leg, towards the front.
    let side = vec2f(-d.y, d.x) / len * sign(-d.y + 1e-6);
    return hip + 0.5 * d + side * out * front;
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

    // Far enough that the whole body leaves the picture at either end, so the stripes the person
    // leaves reach the edges.
    let reach = aspect * 0.5 + 0.25 * s;
    let x = aspect * 0.5 + reach * way(t);
    // Speed in image heights per second of `t`, and the side the person faces: the way they
    // move, front-on while they stand.
    let dx = (way(t + 0.05) - way(t - 0.05)) * reach * 10.0;
    let facing = clamp(dx * 4.0, -1.0, 1.0);
    let front = abs(facing);

    let floor_y = 0.035;
    let bone = 0.25 * s;
    let leg = 2.0 * bone;
    // One stride (two steps) is this far. Longer strides when walking fast.
    let stride = s * mix(0.35, 0.62, clamp(abs(dx) * 1.5, 0.0, 1.0));

    // Each foot rests while the body passes over it (60 % of a stride) and swings forward over the
    // rest. Its phase follows the distance from the middle, so a resting foot does not slide,
    // whichever way the person walks.
    var feet: array<vec2f, 2>;
    var reach_of: array<f32, 2>;
    for (var i = 0; i < 2; i++) {
        let c = fract(x / stride + 0.5 * f32(i));
        var ahead = 0.0;
        var lift = 0.0;
        if c < 0.6 {
            ahead = 0.3 - c;
        } else {
            let w = (c - 0.6) / 0.4;
            let e = w * w * (3.0 - 2.0 * w);
            ahead = -0.3 + 0.6 * e;
            lift = 0.06 * s * sin(3.1415927 * w) * front;
        }
        reach_of[i] = ahead / 0.3;
        // Seen from the front, the feet stand apart instead of in front of each other.
        let apart = (f32(i) - 0.5) * 0.09 * s * (1.0 - front);
        feet[i] = vec2f(x + ahead * stride * front + apart, floor_y + lift);
    }
    // The hip as high as the resting foot lets it: lowest at the long step, highest above it.
    var standing = 0.0;
    for (var i = 0; i < 2; i++) {
        let off = abs(feet[i].x - x);
        if feet[i].y <= floor_y + 1e-4 {
            standing = max(standing, sqrt(max(0.97 * 0.97 * leg * leg - off * off, 0.0)));
        }
    }
    // The leg runs from 0.03 below the hip to the ankle, 0.03 above the foot.
    // Half of the drop a stiff leg would have: the knee and the ankle take up the rest.
    let hip_y = floor_y + 0.06 * s + 0.97 * leg - 0.5 * select(0.0, 0.97 * leg - standing, standing > 0.0);
    // Leaning into the walk.
    let lean = 0.06 * s * clamp(dx * 2.0, -1.0, 1.0);
    let hip = vec2f(x, hip_y);
    let neck = hip + vec2f(lean, 0.3 * s);
    let head = neck + vec2f(0.4 * lean + 0.01 * s * facing, 0.085 * s);

    var d = capsule(p, hip + vec2f(0.0, 0.02 * s), neck - vec2f(0.0, 0.03 * s), 0.07 * s);
    // A coat, wider towards the hem.
    let hem = clamp((hip.y + 0.06 * s - p.y) / (0.2 * s), 0.0, 1.0);
    let coat = capsule(p, hip + vec2f(0.0, 0.08 * s), hip - vec2f(0.0, 0.08 * s), (0.075 + 0.035 * hem) * s);
    d = smin(d, coat, 0.03 * s);
    d = smin(d, length(p - head) - 0.062 * s, 0.03 * s);

    for (var i = 0; i < 2; i++) {
        let apart = (f32(i) - 0.5) * 0.07 * s * (1.0 - front);
        let top = hip + vec2f(apart, -0.03 * s);
        let ankle = feet[i] + vec2f(0.0, 0.03 * s);
        let knee = knee_of(top, ankle, bone, facing);
        // The toes point the way the person faces, and up a little as the heel lands.
        let toe = ankle + vec2f(0.075 * s * facing, -0.02 * s + 0.02 * s * max(0.0, reach_of[i]) * front);
        d = smin(d, capsule(p, top, knee, 0.046 * s), 0.02 * s);
        d = min(d, capsule(p, knee, ankle, 0.035 * s));
        d = smin(d, capsule(p, ankle, toe, 0.022 * s), 0.01 * s);

        // The arm swings with the other leg. Now and then one is flung up.
        let fi = f32(i);
        let other = reach_of[1 - i];
        let fling = smoothstep(0.6, 0.92, sin(t * (0.41 + 0.13 * fi) + fi * 2.3));
        let shoulder = neck + vec2f((fi - 0.5) * 0.09 * s * (1.0 - front), -0.04 * s);
        // Angle from straight down, positive towards the side the person faces.
        let swing = 0.45 * other * front;
        let up = mix(swing, 2.6 - 0.3 * fi, fling);
        let side = select(1.0, facing, front > 0.2);
        let toward = select(vec2f(sign(fi - 0.5), 0.0), vec2f(side, 0.0), front > 0.2);
        let upper = vec2f(sin(up) * toward.x + (1.0 - front) * 0.25 * sign(fi - 0.5) * (1.0 - fling), -cos(up));
        let elbow = shoulder + normalize(upper) * 0.16 * s;
        let bend = up + 0.35 + 0.3 * max(0.0, other) * front;
        let lower = vec2f(sin(bend) * toward.x + (1.0 - front) * 0.2 * sign(fi - 0.5) * (1.0 - fling), -cos(bend));
        let hand = elbow + normalize(lower) * 0.15 * s;
        d = smin(d, capsule(p, shoulder, elbow, 0.03 * s), 0.02 * s);
        d = min(d, capsule(p, elbow, hand, 0.026 * s));
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
