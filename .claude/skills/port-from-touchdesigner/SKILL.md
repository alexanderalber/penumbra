---
name: port-from-touchdesigner
description: "Port a TouchDesigner project (.toe or .tox) to penumbra: expand and dump it as text, read the network, rebuild it idiomatically through commands (TOPs, CHOPs, SOPs and COMPs to penumbra nodes, GLSL TOPs to WGSL shaders, Python expressions and Execute DATs to bindings, chan nodes or script.python), report what has no counterpart, then render headless and compare with a TouchDesigner screenshot or recording. Use when someone wants a TouchDesigner project, scene or component in penumbra, asks how a TouchDesigner operator maps to penumbra, or hands over a .toe or .tox file."
---

# Porting a TouchDesigner project

There is no converter. A port is a rebuild by hand: read what the TouchDesigner network does,
then build the same result from penumbra nodes through commands. The result is usually smaller
than the original, because one penumbra node often does what took TouchDesigner three.

The commands go over penumbra's control channel: `penumbra-cli ctl <method> '<json>'`, the MCP
tools (`command_run`, `command_batch`), or a headless script (step 7). `node.kinds {kind}`
lists a node type's params, inputs and outputs; `node.kinds {kind, param}` explains one param.
The node reference is `docs/reference/nodes.md` in the penumbra repository, the shader guide
`docs/shaders.md`, the script guide `docs/scripts.md`.

## Ground rules

- **Work on a copy.** Never write into the folder of the user's `.toe`.
- **Rebuild the result, not the operators.** A Speed CHOP into a Limit CHOP set to loop is one
  `chan.integrator` with `limit` `loop`. Rectangles drawn into a mask and read back by Blob
  Track are one `tex.shapes`.
- **Check every node type and param before using it:** `node.kinds {kind}`. To find the
  counterpart of a TouchDesigner operator, `node.kinds {search: "<TD name>"}` finds types by
  TouchDesigner names (`lag` finds `chan.filter`, `glsl` finds `tex.shader`).
- **Look at every result.** `node.snapshot` writes a PNG; open it. A reply with `ok` is not a
  picture that looks right.
- **Keep a list** of what was rebuilt, approximated, assumed and left out. The user gets it at
  the end (step 7).

## 1. Expand and dump

`td_dump.py`, beside this file, turns the project into one text file. It needs TouchDesigner's
own `toeexpand.exe` (in TouchDesigner's `bin` folder), which it finds by itself; it is not
bundled with penumbra.

    python .claude/skills/port-from-touchdesigner/td_dump.py path/to/project.toe --work <scratch folder>

It copies the `.toe` into the work folder, expands it there, unpacks components saved "load on
demand" and writes `dump.txt`. The summary it prints lists the operator types, the DATs with
text (GLSL, Python), the files named in params and the palette components. Run it without
arguments for the options; `--all` also dumps the insides of TouchDesigner's palette
components.

How to read the dump:

- **One block per operator:** path, `[FAMILY:type]` (`TOP:glsl`, `CHOP:lfo`), the flags that
  matter (`display`, `render`, `bypass`, `lock`, `export`), `in:` with the input number and the
  source, then the params.
- **Only params changed from TouchDesigner's default are saved.** A Noise TOP with no
  `resolutionw` is at TouchDesigner's default size. Look defaults up in TouchDesigner's
  operator documentation (docs.derivative.ca) when they matter.
- **Expressions:** `` name = `expr` (expression; constant when off: v) `` means the param
  follows the expression. `v` is only the fallback, not what the expression gave. "expression
  stored but off" means the plain value applies.
- **Exports:** "driven by a CHOP export" and "params driven by exports from" mean a CHOP
  writes that param every frame. The export table of that CHOP names the channels.
- **Node references in params are wires too.** `top = posBuffer` (Feedback, Select),
  `material = phong1`, `instancetop = posBuffer`, `camera = cam1`, `geometry = *` connect
  operators without a drawn wire.
- **DAT text** (shaders, scripts) is printed in full up to 4000 characters; longer ones name
  their file in the expanded folder.
- **Locked data** (`.data`, `.chop` files) is binary. Ask the user for an export when a
  locked operator carries content.

## 2. Read the network

1. **Start at the output** and follow the inputs backwards: the Out TOP or Window COMP, or the
   operator the project's root container shows (`top = ./out1` on `/project1`). Anything no
   output reaches is a leftover. List it, do not port it.
2. **Find the clocks:** `absTime`, `me.time`, LFO, Timer, Speed and Count CHOPs, Execute DATs.
   They decide what moves and how fast. `/local/time` at the end of the dump holds the frame
   rate.
3. **Find the assets:** the summary's "files named in params". Copy them into the penumbra
   project folder and name them by relative path.
4. **Ask for a reference:** a screenshot or a short recording of TouchDesigner's output, the
   same frame size as the port. Without one the comparison in step 7 is by the network alone.
5. **Write the plan** before building: each piece of the network, its penumbra counterpart or
   "none", and the assumptions.

## 3. Differences that bite in every port

| | TouchDesigner | penumbra |
|---|---|---|
| colours | display values (sRGB numbers) | linear light: convert colours and ramp keys with the sRGB curve, `c / 12.92` up to 0.04045, else `((c + 0.055) / 1.055) ^ 2.4` |
| ramp blends | between keys in display values | between keys in linear values: two converted keys give a lighter middle. Sample 8 or more keys along TouchDesigner's blend and convert each |
| luminance thresholds | on display values | on linear values: a threshold of 0.45 becomes about 0.17 |
| blends on colours (Composite `color`, soft light) | on display values | on linear values; a `tex.level` with `gamma` 2.2 before and 1/2.2 after gives TouchDesigner's look |
| layer order | Over, Composite: the first input lies on top | `tex.composite`: `in0` lies at the bottom; wire TouchDesigner's inputs in reverse order. Check `subtract` and `difference` with a snapshot |
| picture y | 0 at the bottom, +y up | `v.uv` 0 at the top; `tex.transform` `translate` +y moves down |
| 3D axes | y up, the camera looks along -z | z up: TouchDesigner's (x, y, z) is (x, -z, y). Rotations: TD `rx` stays `rx`, TD `ry` becomes `rz`, TD `rz` becomes minus `ry`; with more than one axis the order changes too, so check a snapshot. Or keep a 3D network in TouchDesigner's frame and put a `geo.transform` with `rotate` [90, 0, 0] before the render: that turns (x, y, z) into (x, -z, y). Camera and lights still need converting |
| camera field of view | horizontal by default | `view.camera` `fov` is vertical: `2 * atan(tan(h / 2) / aspect)`; 45° across at 16:9 is 26.2° |
| generator size | 256 x 256 when not set (Noise, Ramp, Constant TOPs) | 1920 x 1080 by default; set `width`, `height`, and `keep_size` for a simulation buffer |
| float textures | 32-bit float when chosen | RGBA 16-bit float: about 3 decimal digits. Tiny per-frame steps (a life losing 0.0006 per frame near 1) round away; dither them or rescale the value |
| time | per frame, at the project's rate | params and scripts work in seconds; the show runs at 60 frames a second. A step of 0.004 per frame at 60 fps is 0.24 per second |
| resolution of the final picture | the Out TOP's or Window's | the size of the last node; an `out.projector` shows it, a headless run snapshots it |

## 4. Mapping table

Check each row with `node.kinds {kind}` for the params. "none" rows are in "What has no
counterpart" below.

| TouchDesigner | penumbra | notes |
|---|---|---|
| Movie File In TOP | `tex.image_file` (stills), `tex.video_file` (videos, GIFs) | |
| Video Device In, NDI In, Syphon Spout In TOP | `tex.video_in`, `tex.ndi_in`, `tex.spout_in` | |
| Constant, Noise, Ramp TOP | `tex.constant`, `tex.noise`, `tex.ramp` | Noise: `frequency` is cells per picture height, start at 1 / period and tune by eye. `amplitude` 1 spans less than TouchDesigner's amp 1: compare the min and max that `node.snapshot` returns. Ramp: the keys DAT's rows become `keys` lines `pos r g b a` in linear. The ring types swap names: TD `circular` (rings) is `radial`, TD `radial` (once around) is `circular` |
| Rectangle, Circle TOP | `tex.primitive` | many shapes with tags: `tex.shapes` |
| Text TOP | `tex.text` | a changing text: bind `text` to a channel with a format (`node.bind {param, from, format: "{.0}"}`) or to a table cell |
| Level, HSV Adjust, Monochrome, Threshold, Limit TOP | `tex.level`, `tex.hsv`, `tex.monochrome`, `tex.threshold`, `tex.limit` | |
| Math TOP | `tex.level` (`black`, `white`, `brightness`), `tex.channel_mix` (`bias`) | multiplying by a channel: a shader param bound to it |
| Lookup TOP | `tex.lookup` with a `tex.ramp` into `lut`, or its `palette` | |
| Blur, Edge, Slope, Displace, Remap, Lens Distort TOP | `tex.blur`, `tex.edge`, `tex.slope`, `tex.displace`, `tex.remap`, `tex.lens_distort` | `tex.blur` `size` is a radius of three standard deviations; compare by eye |
| Transform, Fit, Flip, Resolution, Tile TOP | `tex.transform`, `tex.fit`, `tex.flip`, `tex.resolution`, `tex.tile` | Transform translate is in picture sizes, +y down |
| Over, Under, Composite, Add, Multiply, Subtract, Inside, Outside TOP | `tex.composite` with `op` | layer order reversed (above); TouchDesigner's "Swap Operation Order" is `swap`. The output has the size of `in0`; TouchDesigner's `size` param may pick another input, so fit the layers first |
| Cross, Switch TOP | `tex.cross`, `tex.switch` | Switch with blend: `blend` on; `inactive: skip` stops the chains not shown |
| Feedback TOP | `tex.feedback` | TouchDesigner's input goes into `initial`; the operator its `top` param names is wired into `previous` |
| Cache, Time Machine TOP | `tex.cache`, `tex.time_machine` | |
| Chroma Key, Matte, Reorder, Channel Mix TOP | `tex.chroma_key`, `tex.matte`, `tex.channel_mix` | |
| Blob Track, Optical Flow TOP | `tex.blob_track`, `tex.optical_flow` | |
| CHOP to TOP, TOP to CHOP | `tex.from_chan`, `chan.from_tex` | |
| GLSL TOP | `tex.shader` | step 5 below |
| Render TOP, Render Select TOP | `tex.render` | geometry, lights and camera are wires into `geo`, `lights`, `camera`; several: `geo.merge`, `light.merge`. Render Select of a Render TOP's first colour buffer is `out`; of a GLSL TOP's buffer N, the pass for buffer N (step 5) |
| Null, Select, In, Out TOP | `tex.null`, `tex.select` (`node`), `tex.in`, `tex.out` | usually just a wire |
| Kantan Mapper, Stoner | `tex.mapper` into `out.projector` | the corners are set again on the real wall |
| Window COMP | `out.projector` (`display`, or `window` for a preview) | |
| Movie File Out TOP | `out.video` (`record`, `file`) | |
| Constant CHOP | `chan.constant` | `name0`, `value0` as in TouchDesigner; `rows` for more |
| LFO CHOP | `chan.lfo` | `frequency` becomes `rate`, `amp` `amplitude`, type `tri` `triangle`; check where the wave starts |
| Noise CHOP | `chan.noise` | |
| Math CHOP | `chan.math` (`op`, `b`) | a from-range/to-range is a `subtract`, a `multiply` and an `add`; or do the sum where the value is used |
| Lag, Filter CHOP | `chan.filter` | Lag: `type` `first_order` with `rise` and `fall` |
| Speed CHOP | `chan.integrator` | with Limit loop: `limit` `loop`, `min`, `max` |
| Limit CHOP | `chan.math` `min`/`max` (clamp) or `modulo` (loop), `floor` (quantise) | |
| Merge, Select, Rename, Null CHOP | `chan.merge`, `chan.select`, `chan.rename`, `chan.null` | |
| Trail, Slope, Trigger, Timer CHOP | `chan.trail`, `chan.derivative`, `chan.envelope`, `chan.clock` | |
| Count, Logic, Pattern, Cross, Delay, Hold CHOP | `script.python`, or `chan.math` where one op does it | |
| Keyboard, Mouse, MIDI, OSC, DMX In CHOP | `chan.keyboard_in`, `chan.mouse_in`, `chan.midi_in`, `chan.osc_in`, `chan.dmx_in` | |
| OSC, MIDI, DMX Out CHOP | `chan.osc_out`, `chan.midi_out`, `out.dmx` | |
| Audio File In, Audio Device In, Audio Device Out CHOP | `audio.file`, `audio.device_in`, `out.audio` | |
| Audio Spectrum CHOP, Audio Analysis component, Envelope CHOP | `chan.audio_spectrum`, `chan.beat_analysis`, `chan.audio_envelope` | |
| DAT to CHOP | `chan.from_table` | |
| Table, File In DAT | `table.text`, `table.file` | |
| Execute, CHOP Execute, DAT Execute DAT | `script.python` | step 6 below |
| Box, Sphere, Grid, Rectangle SOP | `geo.primitive` (`box`, `sphere`, `plane` with `cells`) | the plane stands upright facing -y |
| File In SOP | `geo.file` | |
| Transform, Merge SOP | `geo.transform`, `geo.merge` | |
| Copy SOP, Geo COMP instancing from a CHOP or DAT | `geo.instance` | a table with columns `tx ty tz rx ry rz sx sy sz`, axes converted |
| Geo COMP instancing from TOPs | `geo.tex_instance` | inputs `position`, `aim`, `scale`, `color`, one texel per copy in the order TouchDesigner uses (by instance index), also for a ramp read as the colour. `scale` acts on the model before `aim` turns it; unwired it is 1, an unwired `color` white. `aim` turns the model's +y: build the model with its forward axis (TouchDesigner's `instancerottoforward`) on +y. One axis scaled by one channel (`instancesz = a`) needs a small shader that makes the scale picture. `extent` for framing |
| Geo COMP | `geo.transform` and `geo.assign_material` | the SOP with the render flag inside is the geometry |
| Camera COMP | `view.camera` | `position` and `target` in z-up metres; `projection` `orthographic` with `size` |
| Light COMP | `light.point`, `light.spot` (`angle`), `light.directional` | Ambient and Environment Light COMP: `light.ambient`, `light.environment` |
| Constant, Phong, PBR, Wireframe MAT | `mat.constant`, `mat.phong`, `mat.pbr`, `mat.wireframe` | Phong `diffr diffg diffb` is `color`; set `specular` and `shininess` by the reference, since the defaults differ |
| Base, Container COMP | `comp.base` | custom params: the `expose` param |

## 5. GLSL TOP to WGSL

`docs/shaders.md` has the GLSL to WGSL language table. TouchDesigner's names:

| TouchDesigner GLSL | penumbra WGSL |
|---|---|
| `out vec4 fragColor; ... fragColor = TDOutputSwizzle(c);` | `@fragment fn main(v: PnVertex) -> @location(0) vec4f { ... return c; }` |
| `vUV.st` | `vec2f(v.uv.x, 1.0 - v.uv.y)` |
| `sTD2DInputs[0]` to `[3]` | `in0` to `in3`, wired into the node; more inputs: none, pack them |
| `texture(sTD2DInputs[0], uv)` | `textureSampleLevel(in0, pn_clamp, vec2f(uv.x, 1.0 - uv.y), 0.0)` |
| `texelFetch(sTD2DInputs[0], ivec2(gl_FragCoord.xy), 0)` | `textureLoad(in0, vec2i(v.pos.xy), 0)` |
| `uTDOutputInfo.res.zw` | `pn.resolution` |
| `uniform float uX;` with `uniname0`, `value0x` | `// @param x float = <value>` and `params.x`; bind it where `value0x` had an expression. The node's own param names (`file`, `resolution`, `width`, `height`, `keep_size`) are refused |
| `absTime.seconds`, `absTime.frame` in a uniform | `pn_time.seconds`, `pn_time.frame` in the shader |
| `absTime.stepSeconds` | a param of 1/60 |
| `TDSimplexNoise`, `TDPerlinNoise` | none built in: port a simplex noise (Ashima Arts' 3D simplex noise, MIT, is the usual one) or use `pn_noise` (another look) |
| `TDHSVToRGB`, other `TD...` helpers | write the function in the shader |

Texel coordinates in a simulation: TouchDesigner's row 0 is the bottom, penumbra's the top.
Inside a loop of buffers that only read each other this does not matter; flip only where a
buffer meets a picture.

**A compute shader** (`mode = compute`, `numcolorbufs` N, `imageStore(mTDComputeOutputs[k],
...)`) becomes N `tex.shader` passes, one per output buffer, each writing only its own pixel:

1. One pass per buffer, in the order they depend on each other. When buffer 1 uses the new
   buffer 0, pass 1 reads pass 0's output.
2. `gl_GlobalInvocationID.xy` is `vec2i(v.pos.xy)`. The dispatch size times the work group
   size is the pass size: `resolution` `custom`, `width`, `height`, `keep_size` on.
3. A buffer that reads its own last frame (TouchDesigner's Feedback TOP on it) sits in a
   `tex.feedback`: its start state into `initial`, the pass's output into `previous`, and the
   feedback's output into the pass.
4. TouchDesigner reads the buffers through Render Select TOPs (`colorbufindex` N). Each one
   is the pass for buffer N: wire that pass where the Render Select was.
5. Scattered writes (`imageStore` to another pixel), atomics and shared memory have no
   counterpart. Report them; rethink as a gather where possible.

Save each pass as a `.wgsl` file in the project folder and set the node's `file`. A shader that
does not compile shows the message on the node (`node.info`).

## 6. Python to penumbra

- **`op('x')['chan']` in a param:** `node.bind {param: "/node.param", from: "/x", channel:
  "chan"}`. One component of a vector: `"/t.translate.0"`, or `component` in a batch.
- **Arithmetic on channels** (`op('lfo1')['chan1'] * 2 + 1`): a `chan.math` chain, a shader
  param that does the sum, or `script.python`.
- **Pure numbers** (`1280/2500`): work them out and type the value.
- **`op('table')[r, c]`:** a binding to the cell (`node.bind {param, from, cell: {row, col}}`).
- **`parent().par.X`, custom params:** an `expose` line on the `comp.base`.
- **Execute DATs** (`onFrameStart`), **CHOP Execute** (`onValueChange`), **DAT Execute**:
  one `script.python` each. `cook(cx)` runs every frame (`cook` `every frame`, or `auto` when
  the code reads `cx.time`). Module globals keep state between calls. Channels come in on
  `chan0` and `chan1`, tables on `table0` and `table1`.
- **A script that sets another operator's param** (`op('fit1').par.tx = x`): the script puts
  out a channel (`cx.out["tx"] = x`) and the param is bound to it.
- **Per-frame steps** (`x += 0.004`): write them per second with the time since the last call,
  so the motion does not depend on the frame rate.
- **Extensions and modules:** a `.py` file in the project folder, named by `file`; the
  script's folder is on `sys.path`. Other packages (numpy) work when the Python penumbra found
  has them (`script.info`).

## What has no counterpart

Report each with what it did and what stands in, if anything:

- **Panel COMPs** (Button, Slider, Field, custom UIs): `chan.button` and `chan.slider` on the
  node, a control page in the browser (`chan.http_in`), keys, MIDI or OSC. A full panel UI:
  none.
- **Palette components** (the summary lists them): rebuild what they do from their insides
  (`--all`); the mappers are `tex.mapper`. TouchDesigner's own helpers (`docsHelper`) are
  skipped.
- **SOP modelling:** Trace, Polyreduce, Skin, Line, Circle, Tube, Noise, Particle SOPs. For a
  fixed shape, write an OBJ (a short stdlib Python script) and load it with `geo.file`. For a
  traced picture, a `geo.primitive` plane with the picture as its texture (a `mat.constant`'s
  `texture` input) casts the cut-out shadow by its alpha.
- **Replicator COMP, Script TOP/CHOP/SOP, Engine COMP, Web Render TOP, Bullet and other
  solvers, GLSL MAT:** none; a shader after the render (`tex.render`'s `data` output) stands in
  for some GLSL MATs.
- **Timeline keyframes and Animation COMPs:** `chan.sequence` for timed pieces, otherwise
  `script.python`.

## 7. Build, render, compare

**Build** with `node.batch {ops: [...], label}` so the port is one undo step. Ops: `create`
`{path, kind, params}`, `connect` `{from, output, to, input}`, `bind` `{param, from, channel}`,
`set` `{param, value}`. In a batch, `input` and `output` are numbers: the place of the input in
the type's list in `node.kinds`, from 0. A name there fails the whole batch. A feedback loop:

    {"op": "connect", "from": "/start", "to": "/loop", "input": 0}
    {"op": "connect", "from": "/step", "to": "/loop", "input": 1}

(`tex.feedback`'s inputs are `initial`, then `previous`.) `node.connect` takes names instead
(`"input": "previous"`).

**Render headless** without touching a running app. Copy `penumbra.exe` out of the build folder
(a running exe cannot be rebuilt), give the run its own port and data folder, and pass a script:

    PENUMBRA_PORT=9890 LOCALAPPDATA='<scratch>\appdata' \
        <scratch>/bin/penumbra.exe --headless port.jsonl --project '<scratch>\port' --size 1280x720

`port.jsonl` holds one command per line, `{"wait": seconds}` to let the show run, and `$HERE`
for the project folder:

    {"method": "node.batch", "params": {"ops": [...], "label": "port"}}
    {"wait": 10}
    {"method": "node.snapshot", "params": {"path": "/out", "file": "$HERE/snap/out_10s.png"}}
    {"method": "node.list", "params": {"recursive": true}}

An empty folder becomes a new project. Each reply is printed; read the errors and warnings of
every node (`node.info {path}` shows a node's status).

Three things to know in a headless run:

- **Shaders compile in the background.** Right after the batch, `node.info` of a shader may
  show a job running. Wait a few seconds (`{"wait": 3}`), then `node.reset` every feedback
  loop, so a simulation starts from its first frame.
- **Watch the final node.** Nothing wants `/out` without an `out.projector`, so its chain does
  not cook between snapshots, and the first snapshot after a long wait may miss a render.
  `node.watch {"path": "/out"}` after the batch makes it cook every frame.
- **The show clock runs from the start of the run.** For a picture after exactly N seconds,
  `cook.stepping {"on": true}` after the reset, then `cook.step {"frames": 60 * N}` (at most
  3600 per step).

**Compare** against the reference picture or recording:

- Take snapshots at the moment the reference shows: for a simulation after the same seconds.
  `wait` is wall-clock time; `cook.step` counts show frames.
- Put the two pictures side by side and look: shape, density, colours, motion, framing.
- Compare numbers where the eye is unsure: `node.snapshot` returns the mean colour in linear
  values; convert TouchDesigner's display values first.
- Chaotic content (feedback, simulations) matches in kind, not pixel by pixel. Run it longer
  than the reference to see that it neither dies out nor blows up.
- Fix one difference at a time and snapshot again.

**Report** to the user: the penumbra network in a few lines, a picture of the port beside the
reference, the list of rebuilt, approximated, assumed and left-out pieces, and the timing
(`cook.meter` in a running app).
