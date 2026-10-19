// A `tex` texture (linear light, premultiplied alpha) to Y'CbCr planes in a storage buffer, as
// a video encoder takes them: BT.709 matrix, video range, the sRGB curve (the inverse of what
// video.wgsl applies to BT.709 files, so a recording plays back as it was shown).
//
// One invocation converts a block of 8 x 2 output pixels and writes whole 32-bit words: four
// 8-bit or two 16-bit samples each. Rows are padded to `strides` samples, a multiple of 8 for
// luma and alpha, of 4 for subsampled chroma. Chroma is sited as MPEG has it: co-sited with the
// left luma sample horizontally ([1 2 1] / 4 filter), for 4:2:0 between the two rows.
//
// Without alpha the picture is taken over black (the premultiplied colour as it is); with
// alpha the colour is unpremultiplied and alpha written as a plane of its own (full range).
// The output size may differ from the texture's: it is sampled bilinearly.

struct Conv {
    size: vec2u,
    // Chroma subsampling as shifts: (1, 1) 4:2:0, (1, 0) 4:2:2, (0, 0) 4:4:4.
    shift: vec2u,
    // Plane starts in 32-bit words: Y', Cb, Cr, A.
    offsets: vec4u,
    // Samples per row: luma (and alpha), chroma.
    strides: vec2u,
    bits: u32,
    // 1: two bytes per sample.
    wide: u32,
    alpha: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<uniform> c: Conv;
@group(0) @binding(1) var src: texture_2d<f32>;
@group(0) @binding(2) var smp: sampler;
@group(0) @binding(3) var<storage, read_write> planes: array<u32>;

const KR: f32 = 0.2126;
const KB: f32 = 0.0722;

// NaN and infinities as 0, by their bits (a comparison may be optimised away).
fn clean(v: f32) -> f32 {
    if ((bitcast<u32>(v) & 0x7f800000u) == 0x7f800000u) {
        return 0.0;
    }
    return v;
}

fn oetf(v: f32) -> f32 {
    if (v <= 0.0031308) {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

// Y', Cb, Cr (centred on 0) and alpha of output pixel `p`, clamped into the picture.
fn pixel(p: vec2i) -> vec4f {
    let q = clamp(p, vec2i(0), vec2i(c.size) - 1);
    let uv = (vec2f(q) + 0.5) / vec2f(c.size);
    let s = textureSampleLevel(src, smp, uv, 0.0);
    let a = clamp(clean(s.a), 0.0, 1.0);
    var rgb = vec3f(clean(s.r), clean(s.g), clean(s.b));
    if (c.alpha == 1u) {
        rgb = select(vec3f(0.0), rgb / max(a, 1e-6), a > 0.0);
    }
    rgb = clamp(rgb, vec3f(0.0), vec3f(1.0));
    let e = vec3f(oetf(rgb.r), oetf(rgb.g), oetf(rgb.b));
    let y = KR * e.r + (1.0 - KR - KB) * e.g + KB * e.b;
    return vec4f(y, (e.b - y) / (2.0 * (1.0 - KB)), (e.r - y) / (2.0 * (1.0 - KR)), a);
}

fn top() -> f32 {
    return f32((1u << c.bits) - 1u);
}

fn q_luma(v: f32) -> u32 {
    let s = f32(1u << (c.bits - 8u));
    return u32(round(clamp((16.0 + 219.0 * v) * s, 0.0, top())));
}

fn q_chroma(v: f32) -> u32 {
    let s = f32(1u << (c.bits - 8u));
    return u32(round(clamp((128.0 + 224.0 * v) * s, 0.0, top())));
}

fn q_alpha(v: f32) -> u32 {
    return u32(round(clamp(v, 0.0, 1.0) * top()));
}

// Writes `n` samples (8 or 4) starting at sample `idx` of the plane at word `base`.
fn store(base: u32, idx: u32, values: array<u32, 8>, n: u32) {
    var v = values;
    if (c.wide == 1u) {
        for (var k = 0u; k < n; k += 2u) {
            planes[base + (idx + k) / 2u] = v[k] | (v[k + 1u] << 16u);
        }
    } else {
        for (var k = 0u; k < n; k += 4u) {
            planes[base + (idx + k) / 4u] = v[k] | (v[k + 1u] << 8u) | (v[k + 2u] << 16u) | (v[k + 3u] << 24u);
        }
    }
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3u) {
    let x0 = id.x * 8u;
    let y0 = id.y * 2u;
    if (x0 >= c.strides.x || y0 >= c.size.y) {
        return;
    }
    // Pixels x0 - 1 ..= x0 + 7 of rows y0 and y0 + 1; the one left of the block feeds the
    // chroma filter.
    var px: array<vec4f, 18>;
    for (var r = 0u; r < 2u; r++) {
        for (var k = 0u; k < 9u; k++) {
            px[r * 9u + k] = pixel(vec2i(i32(x0 + k) - 1, i32(y0 + r)));
        }
    }
    for (var r = 0u; r < 2u; r++) {
        var ys: array<u32, 8>;
        var alphas: array<u32, 8>;
        for (var k = 0u; k < 8u; k++) {
            let p = px[r * 9u + k + 1u];
            ys[k] = q_luma(p.x);
            alphas[k] = q_alpha(p.w);
        }
        let at = (y0 + r) * c.strides.x + x0;
        store(c.offsets.x, at, ys, 8u);
        if (c.alpha == 1u) {
            store(c.offsets.w, at, alphas, 8u);
        }
    }
    if (c.shift.x == 0u) {
        // 4:4:4
        for (var r = 0u; r < 2u; r++) {
            var cbs: array<u32, 8>;
            var crs: array<u32, 8>;
            for (var k = 0u; k < 8u; k++) {
                let p = px[r * 9u + k + 1u];
                cbs[k] = q_chroma(p.y);
                crs[k] = q_chroma(p.z);
            }
            let at = (y0 + r) * c.strides.y + x0;
            store(c.offsets.y, at, cbs, 8u);
            store(c.offsets.z, at, crs, 8u);
        }
        return;
    }
    // Four chroma samples per row, at the even pixels x0 + 2j (index 2j + 1 in a row of px).
    var filtered: array<vec2f, 8>;
    for (var r = 0u; r < 2u; r++) {
        for (var j = 0u; j < 4u; j++) {
            let i = r * 9u + 2u * j + 1u;
            let f = 0.25 * px[i - 1u].yz + 0.5 * px[i].yz + 0.25 * px[i + 1u].yz;
            filtered[r * 4u + j] = f;
        }
    }
    let cx = x0 / 2u;
    if (c.shift.y == 1u) {
        // 4:2:0: one chroma row between the two.
        var cbs: array<u32, 8>;
        var crs: array<u32, 8>;
        for (var j = 0u; j < 4u; j++) {
            let f = 0.5 * (filtered[j] + filtered[4u + j]);
            cbs[j] = q_chroma(f.x);
            crs[j] = q_chroma(f.y);
        }
        let at = (y0 / 2u) * c.strides.y + cx;
        store(c.offsets.y, at, cbs, 4u);
        store(c.offsets.z, at, crs, 4u);
        return;
    }
    // 4:2:2: a chroma row for each row.
    for (var r = 0u; r < 2u; r++) {
        var cbs: array<u32, 8>;
        var crs: array<u32, 8>;
        for (var j = 0u; j < 4u; j++) {
            let f = filtered[r * 4u + j];
            cbs[j] = q_chroma(f.x);
            crs[j] = q_chroma(f.y);
        }
        let at = (y0 + r) * c.strides.y + cx;
        store(c.offsets.y, at, cbs, 4u);
        store(c.offsets.z, at, crs, 4u);
    }
}
