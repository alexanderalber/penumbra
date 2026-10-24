# Shaders

`tex.shader` runs a WGSL fragment shader from a file in the project directory. Save the file
and the node reloads it; a version that does not compile leaves the last good one running, with
the compiler's message on the node. Compiling happens off the show's thread, so saving a shader
during a show costs no frames.

## A first shader

Rings over the input, `rings.wgsl`:

```wgsl
// Rings over the input, moving outwards. Edit and save: the node reloads.
// @param rings float = 6 1..30 -- rings across the image height
// @param tint color = [1, 0.8, 0.6, 1]
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let src = textureSampleLevel(in0, pn_clamp, v.uv, 0.0);
    let p = (v.uv - 0.5) * vec2f(pn.resolution.x / pn.resolution.y, 1.0);
    let ring = 0.5 + 0.5 * cos(length(p) * params.rings * 6.2831853 - pn_time.seconds * 2.0);
    return vec4f(src.rgb * params.tint.rgb * ring, src.a);
}
```

The entry point is a fragment function called `main` taking a `PnVertex`. It returns linear,
premultiplied RGBA; values above 1 are kept.

## Params

Comment lines starting with `// @param` declare params. They appear in the node's param panel
under "from the shader", and are set, bound to channels, saved and undone like any other.
The node's own params (`file`, `resolution`, `width`, `height`, `keep_size`) are taken: a
shader declaring one of those names does not load, so call a line width `line`, not `width`.

```
// @param <name> <kind> [= <default>] [<min>..<max>] [-- <doc>]
```

| kind | in WGSL (`params.<name>`) |
|---|---|
| `float` | `f32` |
| `int` | `i32` |
| `bool` | `u32`, 0 or 1 |
| `vec2`, `vec3`, `vec4` | `vec2f` to `vec4f` |
| `color` | `vec4f`, linear, straight alpha |
| `choice(a\|b\|c)` | `i32`, plus a constant per entry: `const <name>_a: i32` |
| `palette` | `PnPalette`: `count` and up to 32 `colours`, linear, straight alpha (see [Palettes](concepts.md#palettes)) |

The default is JSON (`1.5`, `[1, 0, 0, 1]`, `true`), a bare word for a choice, a name or hex
colours for a palette (`blood`, `31aaa9 f8e0a4`); without one it is zero, false, the first
choice or no palette (`count` 0, every colour transparent black). The range is the slider's; typed values may go beyond it.
Examples:

```wgsl
// @param scale float = 4 1..32 -- cells per image height
// @param octaves int = 3 1..8
// @param invert bool
// @param mode choice(add|multiply|screen) = multiply
// @param look palette = blood -- the faces' colours
```

A palette param is read with three functions of the prelude: `pn_palette(params.look, i)` is
colour `i` (wrapping, so any index is safe), `pn_colormap(params.look, t)` blends along the
colours for `t` 0..1 (a gradient), `pn_band(params.look, t)` cuts 0..1 into equal bands, one
per colour. `params.look.count` says how many colours there are; 0 means none was given, which
a shader can take as "use the colour params instead":

```wgsl
// @param c0 color = [1, 0.5, 0.2, 1]
// @param look palette -- instead of c0 and the rest
fn colour(i: i32) -> vec3f {
    if (params.look.count > 0) {
        return pn_palette(params.look, i).rgb;
    }
    return params.c0.rgb;
}
```

`// @group <name>` puts the params declared after it in a section of that name, folded away
until opened; `// @group` alone goes back to the main rows:

```wgsl
// @param scale float = 4 1..32
// @group detail
// @param warp float = 0 0..1
// @param grain float = 0 0..1
```

`// @density 0.5` tells the [resolution analysis](helpers.md#resolution) how much input
resolution the shader needs per unit of its output's: 1 by default, 2 when it magnifies by 2,
0.5 when it shrinks.

## What a shader can use

A prelude is added to every shader (after it, so line numbers in errors stay yours):

```wgsl
struct PnVertex { @builtin(position) pos: vec4f, @location(0) uv: vec2f }  // uv: 0..1, top left
var<uniform> pn: PnFrame;          // pn.resolution (pixels), pn.texel (1 / resolution)
var<uniform> pn_time: PnTime;      // pn_time.seconds, pn_time.frame (show time)
var<uniform> params: Params;       // one field per declared numeric param
var in0, in1, in2, in3: texture_2d<f32>;      // inputs, transparent black when unconnected
var pn_clamp, pn_repeat, pn_mirror: sampler;  // linear filtering, three edge modes
fn pn_noise(p: vec3f, seed: u32) -> f32       // gradient noise, about -1..1, the same on every GPU
fn pn_value_noise(p: vec3f, seed: u32) -> f32
fn pn_hash(p: vec3u) -> vec3u                 // PCG3D
fn pn_unpremultiply(c: vec4f) -> vec4f
fn pn_luma(rgb: vec3f) -> f32                 // Rec. 709 weights, linear light
fn pn_extend(t: texture_2d<f32>, uv: vec2f, mode: i32) -> vec4f
                                              // 0 hold, 1 zero, 2 repeat, 3 mirror outside 0..1
fn pn_area(t: texture_2d<f32>, s: sampler, uv: vec2f, footprint: vec2f) -> vec4f
                                              // mean over `footprint` source texels (downscaling)
fn pn_palette(p: PnPalette, i: i32) -> vec4f  // a palette param's colour i, wrapping
fn pn_colormap(p: PnPalette, t: f32) -> vec4f // 0..1 along its colours, blended
fn pn_band(p: PnPalette, t: f32) -> vec4f     // 0..1 in equal bands, one per colour
```

A shader that reads `pn_time` cooks every frame; one that does not cooks only when its inputs
or params change.

## Size

The node's `resolution` param: `input` takes the size of `in0` (`width` and `height` while
`in0` is unconnected), `custom` always uses `width` and `height`.
