// Draws one projector's share of the surface into its viewport. The triangles come from the CPU,
// already through the homography and the mesh warp; each vertex carries its surface position. Here:
// the soft-edge share where other projectors show the same part of the surface (mirrors
// `blend_weight` in crates/ops/src/out/mapping.rs), the alignment grid, the projector's colour
// correction, and sRGB encoding for surfaces that do not encode themselves. The input is linear light with premultiplied alpha; over
// black that is just its colour, clipped to 1.

struct Look {
    region: vec4f,
    // The colour of the projector's marks; w its rank (test patterns only).
    tint: vec4f,
    tex_size: vec2f,
    gamma: f32,
    // 1: encode to sRGB, 2: blend, 4: pattern instead of the input, 8: the projector's test
    // pattern instead of anything, 16: the resolution overlay over the input, 32: the flat
    // signal instead of anything, 64: colour correction, 128: structured light stripes instead of
    // anything but the flat signal, 256: the gain map, 512: the black lift (both with 64), 1024:
    // the focus pattern as the test pattern (with 8).
    flags: u32,
    peer_count: u32,
    overlay_factor: f32,
    overlay_scale: vec2f,
    peers: array<vec4f, 8>,
    // x, y, width, height in target pixels.
    viewport: vec4f,
    // The most the target shows, in its own pixels per needed projector pixel.
    overlay_cap: f32,
    // A flat signal (sRGB-encoded), uncorrected.
    flat: vec4f,
    // The correction's matrix, row by row (xyz), and the sample count of each tone curve.
    m0: vec4f,
    m1: vec4f,
    m2: vec4f,
    counts: vec4u,
    // The tone curves: sample i of red, green, blue in xyz.
    curve: array<vec4f, 64>,
    // Structured light: the projector's width and height in pixels, the lit stripes' signal,
    // the pattern's index.
    stripes: vec4f,
    // How much of the gain map and of the black lift apply.
    field: vec4f,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;
@group(0) @binding(2) var<uniform> u: Look;
// Per projector pixel: a factor on the drive, and a lift of the black (see `correct`).
@group(0) @binding(3) var gain_map: texture_2d<f32>;
@group(0) @binding(4) var black_map: texture_2d<f32>;

struct V {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs(@location(0) pos: vec4f, @location(1) uv: vec2f) -> V {
    var v: V;
    v.pos = pos;
    v.uv = uv;
    return v;
}

const BORDER: f32 = 1e-4;
const FAR: f32 = 1e9;

// How far `s` lies inside `r`, towards the nearest edge not on the surface's border, in pixels.
fn inset(r: vec4f, s: vec2f) -> f32 {
    if s.x < r.x || s.x > r.x + r.z || s.y < r.y || s.y > r.y + r.w {
        return 0.0;
    }
    var d = FAR;
    if r.x > BORDER { d = min(d, (s.x - r.x) * u.tex_size.x); }
    if r.x + r.z < 1.0 - BORDER { d = min(d, (r.x + r.z - s.x) * u.tex_size.x); }
    if r.y > BORDER { d = min(d, (s.y - r.y) * u.tex_size.y); }
    if r.y + r.w < 1.0 - BORDER { d = min(d, (r.y + r.w - s.y) * u.tex_size.y); }
    return d;
}

fn share(s: vec2f) -> f32 {
    let mine = max(inset(u.region, s), 1e-9);
    var sum = mine;
    let n = min(u.peer_count, 8u);
    for (var i = 0u; i < n; i++) {
        sum += inset(u.peers[i], s);
    }
    let shaped = smoothstep(0.0, 1.0, mine / sum);
    var total = shaped;
    for (var i = 0u; i < n; i++) {
        total += smoothstep(0.0, 1.0, inset(u.peers[i], s) / sum);
    }
    var w = 1.0;
    if total > 0.0 { w = shaped / total; }
    return pow(w, u.gamma);
}

// Lines every sixteenth of the surface width (square cells), the region's outline and the
// surface's centre lines, about two projector pixels wide.
fn grid(s: vec2f) -> f32 {
    let px = s * u.tex_size;
    let cell = u.tex_size.x / 16.0;
    let f = abs(fract(px / cell + 0.5) - 0.5) * cell;
    let w = max(fwidth(px), vec2f(1e-6));
    let lines = max(1.0 - smoothstep(vec2f(0.0), w * 1.5, f), vec2f(0.0));
    var g = max(lines.x, lines.y) * 0.8;
    let c = abs(px - u.tex_size * 0.5);
    let centre = 1.0 - smoothstep(vec2f(0.0), w * 2.5, c);
    g = max(g, max(centre.x, centre.y));
    let edge = min(s - u.region.xy, u.region.xy + u.region.zw - s) * u.tex_size;
    let outline = 1.0 - smoothstep(vec2f(0.0), w * 3.0, edge);
    return max(g, max(outline.x, outline.y));
}

// The projector's own test pattern, in its pixels `q` of a `size` image, all in its colour (so
// where projectors overlap each one's marks keep their own channel): square cells (eight down,
// so whole cells meet the top and bottom edges) around the centre, every other one a little
// lighter, a cross and a circle at the centre, the image's edge. Shows keystone, overscan,
// whether squares are square, alignment (lines of two projectors that meet show their mix),
// and which projector is which.
fn test_pattern(q: vec2f, size: vec2f) -> vec3f {
    let tint = u.tint.rgb;
    let cell = size.y / 8.0;
    let g = (q - size * 0.5) / cell;
    var rgb = vec3f(0.0);
    let checker = (i32(floor(g.x)) + i32(floor(g.y))) & 1;
    if checker == 1 {
        rgb = tint * 0.03;
    }
    let line = abs(fract(g + 0.5) - 0.5) * cell;
    if min(line.x, line.y) < 0.75 {
        rgb = tint * 0.45;
    }
    let c = abs(q - size * 0.5);
    let r = length(q - size * 0.5);
    if abs(r - size.y * 0.45) < 1.5 {
        rgb = tint;
    }
    if (min(c.x, c.y) < 1.5 && max(c.x, c.y) < cell * 0.75) {
        rgb = tint;
    }
    let edge = min(q, size - q);
    if min(edge.x, edge.y) < 3.0 {
        rgb = tint;
    }
    return rgb;
}

// Inside a focus patch, at `l` pixels from its corner, `side` pixels square (even): one-pixel
// checker top left, two-pixel checker top right, one-pixel rows bottom left, one-pixel columns
// bottom right. Whether the pixel is lit.
fn focus_patch(l: vec2i, side: i32) -> bool {
    let h = side / 2;
    let right = l.x >= h;
    let low = l.y >= h;
    if !right && !low {
        return ((l.x + l.y) & 1) == 0;
    }
    if right && !low {
        return (((l.x >> 1u) + (l.y >> 1u)) & 1) == 0;
    }
    if !right {
        return (l.y & 1) == 0;
    }
    return (l.x & 1) == 0;
}

// The projector's focus pattern, in its pixels `q` of a `size` image, all in its colour:
// patches of one-pixel checkers and lines at the four corners and the centre, where defocus
// shows first, each framed by a one-pixel line a pixel away; between them a one-pixel grid
// (eight cells down, through the centre) at half strength; the image's edge as two one-pixel
// lines. The patches move towards the centre by the projector's rank (`tint.w`, modulo three),
// so projectors stacked on the same area show theirs side by side.
fn focus_pattern(q: vec2f, size: vec2f) -> vec3f {
    let tint = u.tint.rgb;
    let n = vec2i(max(floor(size), vec2f(1.0)));
    let p = clamp(vec2i(floor(q)), vec2i(0), n - 1);
    let mid = n / 2;
    let cell = max(i32(round(size.y / 8.0)), 4);
    var rgb = vec3f(0.0);
    let d = abs(p - mid) % vec2i(cell);
    if d.x == 0 || d.y == 0 {
        rgb = tint * 0.5;
    }
    let side = max((i32(round(size.y / 9.0)) / 2) * 2, 8);
    let margin = max(side / 4, 6);
    let shift = (i32(u.tint.w + 0.5) % 3) * (side + side / 4);
    let far = n - margin - side;
    var origins = array<vec2i, 5>(
        vec2i(margin, margin + shift),
        vec2i(far.x, margin + shift),
        vec2i(margin, far.y - shift),
        vec2i(far.x, far.y - shift),
        mid - vec2i(side / 2) + vec2i(0, shift),
    );
    for (var i = 0; i < 5; i++) {
        let l = p - origins[i];
        // The frame two pixels out, with a black gap of one pixel inside it.
        if all(l >= vec2i(-2)) && all(l < vec2i(side + 2)) {
            let inside = all(l >= vec2i(0)) && all(l < vec2i(side));
            let frame = any(l == vec2i(-2)) || any(l == vec2i(side + 1));
            rgb = vec3f(0.0);
            if frame || (inside && focus_patch(l, side)) {
                rgb = tint;
            }
        }
    }
    let edge = min(p, n - 1 - p);
    let e = min(edge.x, edge.y);
    if e == 0 || e == 2 {
        rgb = tint;
    }
    return rgb;
}

// Surface pixels per needed projector pixel along the surface's worse axis, times the factor
// for a coarser node upstream, at most the target's own (gpu::projection::Overlay).
fn density(s: vec2f) -> f32 {
    return pn_density(s * u.tex_size, u.overlay_scale, u.overlay_factor, u.overlay_cap);
}

fn srgb(c: vec3f) -> vec3f {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3f(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3f(0.0031308));
}

// Tone curve `ch` at encoded drive `x`, piecewise linear between its samples.
fn tone(x: f32, ch: u32) -> f32 {
    let n = max(u.counts[ch], 2u);
    let t = clamp(x, 0.0, 1.0) * f32(n - 1u);
    let i = min(u32(t), n - 2u);
    return mix(u.curve[i][ch], u.curve[i + 1u][ch], t - f32(i));
}

fn bits(side: u32) -> u32 {
    if side <= 1u {
        return 1u;
    }
    return firstLeadingBit(side - 1u) + 1u;
}

// Whether projector pixel `p` of a `size` image is lit in scan pattern `i`, as
// `penumbra_calib::scan::Patterns::lit` and `tex.scan_pattern` have it.
fn stripe_lit(p: vec2u, size: vec2u, i: u32) -> bool {
    if i == 0u {
        return true;
    }
    if i == 1u {
        return false;
    }
    let bx = bits(size.x);
    let by = bits(size.y);
    let k = i - 2u;
    let bit = k / 2u;
    var on = false;
    if bit < bx {
        on = (((p.x ^ (p.x >> 1u)) >> (bx - 1u - bit)) & 1u) == 1u;
    } else if bit < bx + by {
        on = (((p.y ^ (p.y >> 1u)) >> (by - 1u - (bit - bx))) & 1u) == 1u;
    } else {
        return false;
    }
    return on != ((k & 1u) == 1u);
}

// Linear light to the projector's signal: matrix, clamp, the gain map and the black lift at
// projector position `q` (0..1 over the viewport), sRGB encoding, tone curves. The gain works
// on the drive in the projector's own channels, after the matrix: the hot spot and the colour
// cast belong to the projector's light, which the drive scales; and before the curves, which
// only bend the drive into the projector's response.
fn correct(c: vec3f, q: vec2f) -> vec3f {
    var d = clamp(vec3f(dot(u.m0.xyz, c), dot(u.m1.xyz, c), dot(u.m2.xyz, c)), vec3f(0.0), vec3f(1.0));
    if (u.flags & 256u) != 0u {
        let g = clamp(textureSampleLevel(gain_map, smp, q, 0.0).rgb, vec3f(0.0), vec3f(1.0));
        d *= mix(vec3f(1.0), g, clamp(u.field.x, 0.0, 1.0));
    }
    if (u.flags & 512u) != 0u {
        let l = clamp(textureSampleLevel(black_map, smp, q, 0.0).rgb * u.field.y, vec3f(0.0), vec3f(1.0));
        d = l + (vec3f(1.0) - l) * d;
    }
    let e = srgb(d);
    return vec3f(tone(e.r, 0u), tone(e.g, 1u), tone(e.b, 2u));
}

@fragment
fn fs(v: V) -> @location(0) vec4f {
    var rgb: vec3f;
    // The colour is already the signal (flat, corrected): not encoded again.
    var signal = false;
    // Derivatives in uniform control flow, before any branch.
    let g = grid(v.uv);
    let r = density(v.uv);
    if (u.flags & 32u) != 0u {
        rgb = clamp(u.flat.rgb, vec3f(0.0), vec3f(1.0));
        signal = true;
    } else if (u.flags & 128u) != 0u {
        let size = max(u.stripes.xy, vec2f(1.0));
        let q = (v.pos.xy - u.viewport.xy) / max(u.viewport.zw, vec2f(1.0)) * size;
        let p = vec2u(clamp(floor(q), vec2f(0.0), size - 1.0));
        let lit = stripe_lit(p, vec2u(size), u32(u.stripes.w + 0.5));
        rgb = vec3f(select(0.0, clamp(u.stripes.z, 0.0, 1.0), lit));
        signal = true;
    } else if (u.flags & 1032u) == 1032u {
        rgb = focus_pattern(v.pos.xy - u.viewport.xy, u.viewport.zw);
    } else if (u.flags & 8u) != 0u {
        rgb = test_pattern(v.pos.xy - u.viewport.xy, u.viewport.zw);
    } else if (u.flags & 4u) != 0u {
        rgb = u.tint.rgb * g;
    } else {
        let c = textureSampleLevel(src, smp, v.uv, 0.0);
        rgb = clamp(c.rgb, vec3f(0.0), vec3f(1.0));
        if (u.flags & 2u) != 0u {
            rgb *= share(v.uv);
        }
        if (u.flags & 16u) != 0u {
            rgb = pn_overlay_tint(r, rgb);
        }
        if (u.flags & 64u) != 0u {
            let q = (v.pos.xy - u.viewport.xy) / max(u.viewport.zw, vec2f(1.0));
            rgb = correct(rgb, clamp(q, vec2f(0.0), vec2f(1.0)));
            signal = true;
        }
    }
    if signal {
        // A target that encodes itself (or a linear texture) gets the light that encodes to
        // the signal.
        if (u.flags & 1u) == 0u {
            rgb = pn_srgb_decode(rgb);
        }
    } else if (u.flags & 1u) != 0u {
        rgb = srgb(rgb);
    }
    return vec4f(rgb, 1.0);
}
