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

## From Shadertoy and GLSL

A Shadertoy shader ports line by line. Shadertoy's template for a new shader, and the same in
penumbra:

```glsl
void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;
    vec3 col = 0.5 + 0.5 * cos(iTime + uv.xyx + vec3(0, 2, 4));
    fragColor = vec4(col, 1.0);
}
```

```wgsl
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let fragCoord = vec2f(v.pos.x, pn.resolution.y - v.pos.y);
    let uv = fragCoord / pn.resolution;
    let col = 0.5 + 0.5 * cos(pn_time.seconds + uv.xyx + vec3f(0.0, 2.0, 4.0));
    return vec4f(pow(col, vec3f(2.2)), 1.0);
}
```

Three differences matter in every port:

- **Pixels count from the top.** `v.pos.xy` is the pixel position with 0 at the top left.
  Shadertoy's `fragCoord` has 0 at the bottom left. The first line above flips it, so the rest
  of the code stays as it was.
- **Colours are linear.** Shadertoy writes its numbers straight to the screen. penumbra takes
  them as light and encodes them for the display, so a ported shader looks pale.
  `pow(col, vec3f(2.2))` at the end brings back the look it had on Shadertoy.
- **Alpha is premultiplied.** For a transparent part, multiply the colour by the alpha:
  `vec4f(col * a, a)`. A shader whose alpha stays 1 needs nothing.

The names:

| Shadertoy | penumbra |
|---|---|
| `iTime` | `pn_time.seconds`: show time, stands still while the clock is paused |
| `iFrame` | `pn_time.frame` (an `f32`) |
| `iResolution.xy` | `pn.resolution` (a `vec2f`) |
| `fragCoord` | `vec2f(v.pos.x, pn.resolution.y - v.pos.y)` |
| `fragCoord / iResolution.xy` | `vec2f(v.uv.x, 1.0 - v.uv.y)`; `v.uv` itself runs from the top left |
| `iMouse.xy` | `params.mouse * pn.resolution`, with `// @param mouse vec2` bound to a `chan.mouse_in`'s `x` and `y` (they count from the bottom left, as Shadertoy does) |
| `iChannel0` to `iChannel3` | `in0` to `in3`, wired into the node |
| `texture(iChannel0, uv)` | `textureSampleLevel(in0, pn_repeat, uv, 0.0)`, with `uv` from the top left; `pn_clamp` and `pn_mirror` for the other edge modes |
| `iChannelResolution[0].xy` | `vec2f(textureDimensions(in0))` |
| `fragColor = c;` | `return c;` |
| Buffer A to D | one `tex.shader` per buffer, wired into the next; a buffer that reads itself goes into a `tex.feedback` loop (below) |

The language:

| GLSL | WGSL |
|---|---|
| `float`, `int`, `vec2`, `vec3`, `vec4`, `ivec2`, `mat2` | `f32`, `i32`, `vec2f`, `vec3f`, `vec4f`, `vec2i`, `mat2x2f` |
| `float x = 1.0;` | `let x = 1.0;` when it never changes, `var x = 1.0;` when it does |
| `#define PI 3.14159`, `const float PI = ...` | `const PI = 3.14159;` |
| `float f(vec2 p) { ... }` | `fn f(p: vec2f) -> f32 { ... }` |
| `for (int i = 0; i < 8; i++)` | `for (var i = 0; i < 8; i++)` |
| `float(i)` | `f32(i)`: an `i32` never turns into an `f32` by itself |
| `c ? a : b` | `select(b, a, c)`: the false value comes first |
| `mod(x, y)` | `x - y * floor(x / y)`. WGSL's `x % y` keeps the sign of `x` and differs below 0 |
| `atan(y, x)` | `atan2(y, x)` |
| `dFdx`, `dFdy` | `dpdx`, `dpdy` |
| `col.rg = vec2(1.0);` | one component at a time (`col.x = 1.0;`), or build the vector anew |
| `out` and `inout` params | return the value, or a struct of several |

The same in both: swizzles that read (`p.xy`, `col.bgr`, `uv.xyx`), `fract`, `floor`, `mix`,
`clamp`, `step`, `smoothstep`, `length`, `normalize`, `dot`, `cross`, `sin`, `cos`, `pow`,
`abs`, `min` and `max`. Numbers and vectors mix as in GLSL: `0.5 + 0.5 * v` works.

## From a snippet to a loop that moves with the music

Three steps, each a few minutes, from a shader to a picture that reacts to sound.

**1. The shader.** Save this as `ring.wgsl` in the project folder. Add a `tex.shader` and set
its `file` to `ring.wgsl`. Select the node: the viewer shows a ring, and the row `energy`
appears under "from the shader".

```wgsl
// A ring in the template's colours, transparent around it.
// @param energy float = 0 0..1 -- how far the ring opens; bind it to the music
@fragment
fn main(v: PnVertex) -> @location(0) vec4f {
    let p = (v.uv - 0.5) * vec2f(pn.resolution.x / pn.resolution.y, 1.0);
    let radius = 0.1 + 0.3 * params.energy;
    let ring = 1.0 - smoothstep(0.0, 0.01, abs(length(p) - radius));
    let col = 0.5 + 0.5 * cos(pn_time.seconds + p.xyx + vec3f(0.0, 2.0, 4.0));
    return vec4f(pow(col, vec3f(2.2)) * ring, ring);
}
```

**2. Feedback.** Each frame builds on the one before, so the ring leaves trails:

- a `tex.constant` (black) into the `initial` input of a `tex.feedback`;
- the feedback into a `tex.transform` with `scale` 1.01 and `rotate` 0.5: the trails drift
  outwards and turn;
- into a `tex.level` with `brightness` 0.95: the trails fade within a second or two;
- into `in0` of a `tex.composite`, the shader into its `in1`;
- the composite back into the feedback's `previous`. That wire is drawn dashed.

The composite is the picture. [tex.feedback](reference/nodes.md#texfeedback) and the `feedback`
example show more of what a loop can do.

**3. Sound.** Add an `audio.device_in` and wire it into a `chan.audio_analysis` (see
[Audio](audio.md)). Wire the analysis into a `chan.filter`, which smooths the jumps. Then bind
`energy`: the link button at the end of its row offers the filter's channels, pick `low`. The
ring now opens with every bass note. Without a microphone, a `chan.lfo` with `offset` 0.5
and `amplitude` 0.5 stands in.

The whole spectrum fits into a shader too: `chan.audio_analysis`'s `spectrum` output into a
`tex.from_chan`, that into `in1`. Then `textureSampleLevel(in1, pn_clamp, vec2f(x, 0.5), 0.0).r`
is the loudness at `x`, low frequencies at 0.

To show it on a wall, wire the composite into an `out.projector`, or through a `tex.mapper`
first ([Mapping without a camera](mapping-without-camera.md)).
