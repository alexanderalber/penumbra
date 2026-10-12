// tex.opticalflow: pyramidal Lucas-Kanade, one stage per pass, `stage` set by the node:
//
//   0 prep   the input shrunk to the analysed size, as the square root of its luma (so one
//            amount of texture means about the same in dark and bright parts)
//   1 down   one pyramid level from the next finer one, half the size
//   2 solve  one Lucas-Kanade step at a level: in0 this frame's level, in1 the previous
//            frame's, in2 the motion so far (unconnected: none)
//   3 out    the finest motion with small motions faded out, or shown as colours
//
// Motion is kept in image sizes throughout (x in widths, y in heights, y down), so a coarser
// level's result is the next finer level's start as it is. It means: what is at a pixel now was
// `motion` further up and left in the previous frame.

const PN_FLOW_SOLVE_RIDGE: f32 = 1e-4;
// The structure (smaller eigenvalue) at which the confidence is one half: a little above what
// camera noise alone makes at the analysed size.
const PN_FLOW_TEXTURE: f32 = 1e-5;

fn pn_flow_load(t: texture_2d<f32>, p: vec2i) -> vec4f {
    let last = vec2i(textureDimensions(t)) - 1;
    return textureLoad(t, clamp(p, vec2i(0), last), 0);
}

fn pn_flow_prep(v: PnVertex) -> vec4f {
    let footprint = vec2f(textureDimensions(in0)) * pn.texel;
    let c = pn_area(in0, pn_clamp, v.uv, footprint);
    return vec4f(sqrt(max(pn_luma(c.rgb), 0.0)), 0.0, 0.0, 1.0);
}

fn pn_flow_down(v: PnVertex) -> vec4f {
    // A little wider than the 2 x 2 it replaces, so the coarse levels do not alias.
    let footprint = vec2f(textureDimensions(in0)) * pn.texel + 1.0;
    return vec4f(pn_area(in0, pn_clamp, v.uv, footprint).r, 0.0, 0.0, 1.0);
}

// Value and gradient (central differences, per pixel of the level) at a texel of this frame.
fn pn_flow_now(p: vec2i) -> vec3f {
    let e = pn_flow_load(in0, p).r;
    let gx = pn_flow_load(in0, p + vec2i(1, 0)).r - pn_flow_load(in0, p - vec2i(1, 0)).r;
    let gy = pn_flow_load(in0, p + vec2i(0, 1)).r - pn_flow_load(in0, p - vec2i(0, 1)).r;
    return vec3f(e, 0.5 * gx, 0.5 * gy);
}

// The same anywhere in the previous frame, between texels.
fn pn_flow_before(uv: vec2f, texel: vec2f) -> vec3f {
    let e = textureSampleLevel(in1, pn_clamp, uv, 0.0).r;
    let gx = textureSampleLevel(in1, pn_clamp, uv + vec2f(texel.x, 0.0), 0.0).r
        - textureSampleLevel(in1, pn_clamp, uv - vec2f(texel.x, 0.0), 0.0).r;
    let gy = textureSampleLevel(in1, pn_clamp, uv + vec2f(0.0, texel.y), 0.0).r
        - textureSampleLevel(in1, pn_clamp, uv - vec2f(0.0, texel.y), 0.0).r;
    return vec3f(e, 0.5 * gx, 0.5 * gy);
}

fn pn_flow_solve(v: PnVertex) -> vec4f {
    let dims = vec2f(textureDimensions(in0));
    let p = vec2i(v.pos.xy);
    let guess = textureSampleLevel(in2, pn_clamp, v.uv, 0.0).xy;
    let d = guess * dims;
    let r = clamp(params.window, 1, 8);
    let sigma = max(f32(r) * 0.6, 0.5);
    // Sums of the structure tensor and of gradient times difference, Gaussian weighted. The
    // difference is taken against its mean over the window, so a change of brightness (a flicker,
    // the camera's exposure) is not taken for motion.
    var t = vec3f(0.0);
    var s = vec2f(0.0);
    var gs = vec2f(0.0);
    var es = 0.0;
    var total = 0.0;
    for (var y = -r; y <= r; y++) {
        for (var x = -r; x <= r; x++) {
            let o = vec2f(f32(x), f32(y));
            let w = exp(-0.5 * dot(o, o) / (sigma * sigma));
            let q = clamp(p + vec2i(x, y), vec2i(0), vec2i(dims) - 1);
            let now = pn_flow_now(q);
            let before = pn_flow_before((vec2f(q) + 0.5 - d) / dims, 1.0 / dims);
            let g = 0.5 * (now.yz + before.yz);
            let e = before.x - now.x;
            t += w * vec3f(g.x * g.x, g.x * g.y, g.y * g.y);
            s += w * g * e;
            gs += w * g;
            es += w * e;
            total += w;
        }
    }
    let mg = gs / total;
    t = t / total - vec3f(mg.x * mg.x, mg.x * mg.y, mg.y * mg.y);
    s = s / total - mg * (es / total);
    // Ridge: where there is no texture to follow, the step shrinks towards none, so flat parts
    // keep what the coarser level found instead of amplifying noise.
    let a = t.x + PN_FLOW_SOLVE_RIDGE;
    let c = t.z + PN_FLOW_SOLVE_RIDGE;
    let det = a * c - t.y * t.y;
    var step = vec2f(c * s.x - t.y * s.y, a * s.y - t.y * s.x) / max(det, 1e-12);
    let len = length(step);
    if (len > f32(r)) {
        step *= f32(r) / len;
    }
    // Confidence: the smaller eigenvalue of the tensor, how well the window pins down both
    // directions.
    let half = 0.5 * (t.x + t.z);
    let low = max(half - sqrt(0.25 * (t.x - t.z) * (t.x - t.z) + t.y * t.y), 0.0);
    let confidence = low / (low + PN_FLOW_TEXTURE);
    return vec4f((d + step) / dims, confidence, 1.0);
}

fn pn_flow_hue(h: f32) -> vec3f {
    let k = fract(vec3f(h) + vec3f(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0;
    return clamp(abs(k - 3.0) - 1.0, vec3f(0.0), vec3f(1.0));
}

fn pn_flow_out(v: PnVertex) -> vec4f {
    let f = pn_flow_load(in0, vec2i(v.pos.xy));
    let dims = vec2f(textureDimensions(in0));
    let speed = length(f.xy * dims);
    let t = max(params.threshold, 0.0);
    let keep = select(1.0, smoothstep(0.5 * t, t, speed), t > 0.0);
    let motion = f.xy * keep;
    if (params.colors == 0u) {
        return vec4f(motion, select(0.0, f.z, params.confidence != 0u), 1.0);
    }
    // Direction as hue, speed as brightness, full at 1/80 of the width per frame.
    let m = motion * vec2f(1.0, dims.y / dims.x);
    let bright = clamp(length(m) * 80.0, 0.0, 1.0);
    let hue = atan2(m.y, m.x) / 6.2831853 + 1.0;
    return vec4f(pn_flow_hue(hue) * bright, 1.0);
}

@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    switch params.stage {
        case 0: { return pn_flow_prep(v); }
        case 1: { return pn_flow_down(v); }
        case 2: { return pn_flow_solve(v); }
        default: { return pn_flow_out(v); }
    }
}
