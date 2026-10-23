// Helpers shared by the built-in shaders of this crate, put in front of each of them (as the
// passes of tex nodes get their prelude).

// One triangle covering the target, from the vertex index (0, 1, 2).
fn pn_fullscreen(i: u32) -> vec4f {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    return vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
}

// sRGB-encoded to linear light.
fn pn_srgb_decode(c: vec3f) -> vec3f {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3f(2.4));
    return select(hi, lo, c <= vec3f(0.04045));
}

// The resolution overlay (gpu::projection::Overlay): texels per needed projector pixel along the
// worse axis at texel position `t`, the derivatives scaled by `scale` (target pixels per needed
// projector pixel), times `factor` (for a coarser node upstream), at most `cap`.
fn pn_density(t: vec2f, scale: vec2f, factor: f32, cap: f32) -> f32 {
    let gx = vec2f(dpdx(t.x), dpdy(t.x)) * scale;
    let gy = vec2f(dpdx(t.y), dpdy(t.y)) * scale;
    return min(min(length(gx), length(gy)) * factor, cap);
}

// The overlay's colour for density `r`: green about one to one, red too coarse (below 0.9), blue
// wasted (above 1.3), the limits of ops::res; the picture's brightness shows through.
fn pn_overlay_tint(r: f32, c: vec3f) -> vec3f {
    var tint = vec3f(0.1, 0.85, 0.2);
    tint = mix(vec3f(1.0, 0.1, 0.05), tint, smoothstep(0.87, 0.9, r));
    tint = mix(tint, vec3f(0.15, 0.35, 1.0), smoothstep(1.3, 1.36, r));
    let luma = dot(c, vec3f(0.2126, 0.7152, 0.0722));
    return tint * (0.3 + 0.7 * luma);
}

