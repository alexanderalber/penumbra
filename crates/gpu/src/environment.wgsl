// Baking an environment light (environment.rs): an equirectangular picture (z up, Blender's
// orientation: the middle looks along +x, u grows clockwise seen from above, the top row is
// straight up) into a mip chain of its plain average, a mip chain prefiltered for roughness
// (mip 0 sharp, the last mip roughness 1) and a small irradiance map.

struct Bake {
    // Source texels per target texel (fs_down).
    footprint: vec2f,
    // One source texel in uv (fs_down).
    src_texel: vec2f,
    // fs_prefilter: the roughness of this level, the plain chain's last mip, its mip 0's texels.
    roughness: f32,
    lods: f32,
    texels: f32,
    pad: f32,
};

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;
@group(0) @binding(2) var<uniform> bake: Bake;

const PI: f32 = 3.14159265;

struct V {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> V {
    let x = f32((i << 1u) & 2u);
    let y = f32(i & 2u);
    var v: V;
    v.pos = vec4f(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    v.uv = vec2f(x, y);
    return v;
}

fn direction(uv: vec2f) -> vec3f {
    let phi = (0.5 - uv.x) * 2.0 * PI;
    let el = (0.5 - uv.y) * PI;
    return vec3f(cos(el) * cos(phi), cos(el) * sin(phi), sin(el));
}

fn equirect(d: vec3f) -> vec2f {
    return vec2f(0.5 - atan2(d.y, d.x) / (2.0 * PI), 0.5 - asin(clamp(d.z, -1.0, 1.0)) / PI);
}

// The mean over a target texel's footprint in the source, light only (alpha dropped).
@fragment
fn fs_down(v: V) -> @location(0) vec4f {
    let n = vec2i(clamp(ceil(bake.footprint), vec2f(1.0), vec2f(8.0)));
    var sum = vec3f(0.0);
    for (var y = 0; y < n.y; y++) {
        for (var x = 0; x < n.x; x++) {
            let k = (vec2f(f32(x), f32(y)) + 0.5) / vec2f(n) - 0.5;
            sum += textureSampleLevel(src, samp, v.uv + k * bake.footprint * bake.src_texel, 0.0).rgb;
        }
    }
    return vec4f(sum / f32(n.x * n.y), 1.0);
}

fn hammersley(i: u32, n: u32) -> vec2f {
    return vec2f(f32(i) / f32(n), f32(reverseBits(i)) * 2.3283064e-10);
}

// The light a mirror of `roughness` reflects along each direction: GGX importance sampling
// with normal, view and reflection alike, each sample from the plain chain's mip whose texels
// are as large as the sample's share of the sphere (Karis 2013).
@fragment
fn fs_prefilter(v: V) -> @location(0) vec4f {
    let n = direction(v.uv);
    let a = bake.roughness * bake.roughness;
    let up = select(vec3f(1.0, 0.0, 0.0), vec3f(0.0, 0.0, 1.0), abs(n.z) < 0.999);
    let tx = normalize(cross(up, n));
    let ty = cross(n, tx);
    let count = 64u;
    var sum = vec3f(0.0);
    var weight = 0.0;
    for (var i = 0u; i < count; i++) {
        let xi = hammersley(i, count);
        let phi = 2.0 * PI * xi.x;
        let cos_t = sqrt((1.0 - xi.y) / (1.0 + (a * a - 1.0) * xi.y));
        let sin_t = sqrt(1.0 - cos_t * cos_t);
        let h = tx * (sin_t * cos(phi)) + ty * (sin_t * sin(phi)) + n * cos_t;
        let l = 2.0 * dot(n, h) * h - n;
        let ndl = dot(n, l);
        if ndl <= 0.0 {
            continue;
        }
        let d = (a * a) / max(PI * pow(cos_t * cos_t * (a * a - 1.0) + 1.0, 2.0), 1e-8);
        let pdf = d / 4.0;
        let sample_sa = 1.0 / (f32(count) * pdf + 1e-6);
        let texel_sa = 4.0 * PI / bake.texels;
        let lod = clamp(0.5 * log2(sample_sa / texel_sa) + 1.0, 0.0, bake.lods);
        sum += textureSampleLevel(src, samp, equirect(l), lod).rgb * ndl;
        weight += ndl;
    }
    return vec4f(sum / max(weight, 1e-6), 1.0);
}

// What a white matte surface facing each direction receives, over pi: the source's texels
// (one small mip) each weighted by its share of the sphere and its cosine.
@fragment
fn fs_irradiance(v: V) -> @location(0) vec4f {
    let n = direction(v.uv);
    let size = textureDimensions(src);
    var sum = vec3f(0.0);
    for (var y = 0u; y < size.y; y++) {
        let el = (0.5 - (f32(y) + 0.5) / f32(size.y)) * PI;
        let sa = cos(el) * (2.0 * PI / f32(size.x)) * (PI / f32(size.y));
        for (var x = 0u; x < size.x; x++) {
            let d = direction((vec2f(f32(x), f32(y)) + 0.5) / vec2f(size));
            let c = max(dot(n, d), 0.0);
            if c > 0.0 {
                sum += textureLoad(src, vec2u(x, y), 0).rgb * (c * sa);
            }
        }
    }
    return vec4f(sum / PI, 1.0);
}
