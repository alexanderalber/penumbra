# The Helpers window

Right-click on empty canvas and choose **Helpers**. The window collects what matters when
setting up a show rather than while building one. Everything in it is also a command (named in
the tooltips), so Claude can do the same.

## Outputs

Which displays are connected (size, refresh rate), which one shows the editor, and the output
window open on each: frames per second, missed frames, the longest frame. Below that, every
active `out.projector` with the display it is on, and its colour in the test pattern, and a row
of buttons that switch the test pattern of every projector (see [Test patterns](#test-patterns)).

The show clock follows the vsync of one output window (marked with a dot), or a timer when no
window runs at the show rate. The status bar repeats this in short ("no outputs · timer").

The timer ticks as evenly as a projector's vsync, so timing while developing without a
projector is what it will be at the show. It costs a few percent of one CPU core.

The display ids listed here are what an `out.projector`'s `display` takes. An id names the
display itself, not the order in which the system lists displays, so plugging in another one
changes nothing:

- `AW3225QF [DELA246 #BY4BYZ3]`: monitor name, model (manufacturer and product, as Device
  Manager shows it) and serial number. Such a display is found on whatever connector it is
  plugged into.
- `EPSON PJ [SECA519] @ target 168194`: a display without a serial number (Epson projectors,
  for one). Two of the same model cannot be told apart by anything they report, so the
  connector on the graphics card decides; plug each projector back into the same socket (and
  through the same adapter, if any).

The monitor name alone (`EPSON PJ`), or with the model (`EPSON PJ [SECA519]`), is enough when
it is unique. Hovering an id shows the model, serial number, connector and the name Windows
gives the display (`\.\DISPLAY2`).

When a projector's display is not certainly there, the projector shows nothing rather than
something on the wrong wall: its node shows the error, and the log names the stored id and what
is connected. A display without a serial number that turns up on another connector is not
followed (it may be the other unit of the same model); the error names it, so you can choose it
after checking. A display that drops out for a moment keeps its window for a second.

Projects from before this scheme store ids like `EPSON PJ @ port 4`. Port numbers shifted when
displays were added, so they no longer decide: such an id still works when only one display has
that monitor name (with a warning), and otherwise shows nothing until the display is chosen
again. The error says which display the old number points at today; check it with `test`.

While at least one output window is open, penumbra tells the operating system that the displays
are in use, so Windows does not turn them (and the projectors) off after its idle timeout. The
Outputs section shows this as one line; `output.list` reports it as `keeps_display_awake`. The
request ends when the last output window closes or penumbra exits.

### Test patterns

The `test` param of a projector shows a test pattern in the projector's own pixels instead of
its input, uncorrected and without soft edges. `test_pattern` picks which:

- **align**: square cells, a centre cross and a circle, the image's edge. For keystone,
  overscan, whether squares are square, and lining projectors up.
- **focus**: patches at the four corners and the centre (where defocus shows first), each with a
  one-pixel checker, a two-pixel checker, one-pixel rows and one-pixel columns, over a one-pixel
  grid. Turn the focus ring until all four quarters of the patches stay crisp; when the corners
  and the centre cannot all be sharp, the lens or the throw is the limit, not the focus.
- **motion**: moves on by whole pixels with every frame the output window presents (not by the
  clock), for stutter that comes from the line (HDMI over Ethernet, extenders, the projector's
  processing). In the top lane a bar moves a 240th of the width per frame, in the lane below one
  a single pixel per frame: both should glide evenly; uneven steps are judder, a horizontal break
  in a bar is tearing. Below them a strip of 30 cells, the next one lit each frame (every fifth
  outlined brighter): filmed with a phone in slow motion, a dropped frame shows as a skipped
  cell, a repeated one as a cell lit twice as long. The line `FRAME 001234   LATE 0` counts the
  frames presented since the pattern came on and how many of them penumbra presented late (more
  than one and a half refresh periods after the one before, as `missed` in `output.list`). Stutter
  while `LATE` stays 0 happens after the graphics card.
- **fields**: full white, red, green, blue and black, three seconds each, uncorrected, to find
  dead or stuck pixels. When several projectors show it they take turns (the others black
  meanwhile), so a dead pixel in an overlap is not hidden by another projector's light.

The row **Test pattern on every projector: Off, Align, Focus, Motion, Fields** under the
projector list switches every `out.projector` at once, as one undo step
(`projector.test {on, pattern?, projectors?}` on the control channel). The lit button is what all
of them show.

The colour depends on how many projectors show a test pattern (those with `test` on, not all in
the project): one alone draws in white, the brightest and easiest to read. Two draw in orange and
azure: red comes only from the first, blue only from the second, so lines of the two that meet
add up to white, and lines that miss show both colours side by side. Three draw in red, green
and blue, so each keeps a channel of its own (lines that meet show the mix, all three white);
more go on with yellow, cyan and magenta. A blurred projector shows as a smeared colour while
the others stay crisp. The square beside each projector in the list shows its colour. The focus
patches and the motion pattern's counts sit lower for the second projector and lower again for
the third, so projectors stacked on the same area show theirs side by side. The full fields are
always in their own colours.

## Blend and match

Under the outputs, this section runs the three jobs that make several projectors look like one:
blend weights from the scans, the colour match and the flat field. Each has a button and its few
options; the rest keeps the commands' defaults, and the files go where the commands put them.

- **Scans** lists every folder in the project that holds a scan's maps (`cam_to_proj.exr` and
  `proj_to_cam.exr`, up to four folders deep), with the projector it belongs to, whether it was
  blended and when it was scanned. A scan belongs to the projector its `tex.scan` names, or to the
  projector it is named after (`scan/proj_a` to `/proj_a`). **look again** after scanning
  elsewhere.
- **Blend** blends the checked scans (`scan.blend`, see [scanning](scanning.md#6-blend-scanblend)).
  **ramp** is the width of the soft edge. **into the scan folders** writes the weights into each
  scan's folder, where its `tex.scan` reads them; on by default when a `tex.scan` writes one of
  the scans. Off, numbered files (`weight_0.png`, in the order listed) go into the folder named
  beside it (`objects`).
- **Camera** and **Projectors** choose what the two measurements use: the `tex.video_in` (the
  one the `tex.scan` nodes name, at first) and the active projectors (all, at first).
- **Colour match**: **Match** measures with the camera (see [below](#matching-projectors)).
  **keep the measurements in** also writes one file per projector into the folder, with the
  **note**. **Match from the files** matches from that folder without the camera. **apply** off
  only measures and reports.
- **Flat field**: each projector with the scan to take (the one found for it, at first), **even**
  (brightness and colour on a bare wall, colour only on a relief) and **Measure** (see
  [below](#even-light-across-a-projector-and-black-in-overlaps)). It connects the maps when
  **apply** is on.

While a job runs, a bar shows its step and how far it got, with **Cancel**: a measurement stops
before its next picture, the blend between its steps, and nothing is applied. One job runs at a
time; the others' buttons wait. When it ends, the result stays under its button until the next
run of the same job: its message, its warnings, and what matters of the reply. For the match a
table: per projector the share of its white it keeps, and for white, red, green, blue and the
greys how far the camera reads it from the reference, in 8-bit camera codes (the largest of the
three channels). Above 4 codes on white or a grey is marked; saturated colours match only
roughly, so their numbers run larger.

The match and the flat field edit as you, as one undo step each. A job started by Claude over
the control channel shows here too, with its progress.

On the control channel the same commands run in the background with `"wait": false`: they
answer at once, `job.status` reports the step, the progress and the last reply of each,
`job.cancel` stops the job. `scan.folders` lists the scans. Without `wait` they answer when done,
as before.

## Matching projectors

Projectors of different models, or of different age, differ in brightness, white point, tone
curve and primaries. `projector.match` measures them with a camera and writes each projector's
colour correction (the *colour* params of `out.projector`), so all of them show the same greys
and whites, as bright as the weakest allows.

Set up a `tex.video_in` that sees the projections, with the camera's automatic exposure, white
balance and any dynamic-range processing (Sony: DRO / Auto HDR) off, and an exposure where a
projector's white is not clipped. Let the projectors overlap in the camera's view if they
overlap on the wall: then all of them are measured on the same patch of wall, which cancels the
wall's colour and the camera's vignetting. Then run, for example:

    penumbra-cli ctl projector.match '{"camera": "/cam", "projectors": ["/proj_a", "/proj_b"]}'

It takes about 50 seconds for two projectors; nobody should walk through the picture meanwhile.
Each projector shows flat colours while the others are black; the network is not changed while
it measures. One projector is the reference (the one that keeps the most brightness for all);
the others are bent to respond like it. Through an ordinary camera only comparisons can be
trusted, not absolute values, so the match works in rounds: every other projector shows white
and then a few greys (`grey_levels`, default 3) through its new correction, and the correction
is adjusted until the camera reads them as it reads the reference (`refine` rounds, default 5).
The result lists what was measured, the reference, each round with the remaining difference in
camera codes, a final check of red, green, blue and the greys, and per projector how much peak
brightness the match costs (`peak`). It writes `matrix` and `curves` as one undo step.

The params of `out.projector`: `matrix` mixes the colour (it brings the white points together
and holds back the brighter projector); `curves` maps the result to the signal per channel.
`strength` blends the whole correction: 1 is the full match, 0 the projector as it is; lower
keeps more of a brighter projector's brightness and leaves more of the difference, which across
a soft edge is often hard to see. `gain` dims a projector by hand. `correct` off shows the plain
picture for comparison. The test pattern is never corrected.

`target: "srgb"` makes every projector follow the sRGB curve instead of the reference; use it
only with a camera known to be linear, through an ordinary camera it bakes the camera's own
curve into every projector and tints the whole picture.

Saturated colours match only approximately.

#### Matching from stored measurements

A measurement belongs to the projector, so it can be kept and combined with others later
without the camera. `save` (a folder, relative to the project) also measures a grey ramp per
projector (a few seconds more) and writes one file per projector, named after its node:

    penumbra-cli ctl projector.match '{"camera": "/cam", "save": "measured", "note": "Cinema, Eco"}'

`note` is kept in the files (picture mode, lamp hours, where the projectors stood). The reply's
`saved` lists the files and, under `stored_match_against_this`, how far a match from these files
would land from what the camera run settled on (the largest signal difference per channel over
a few greys): small numbers say the files are good enough to match from.

Later, without a camera, any combination:

    penumbra-cli ctl projector.match '{"from": {"/proj_a": "measured/proj_a.json", "/proj_c": "other/proj_c.json"}}'

or `"from": "measured"` for a folder with a `<node name>.json` per projector (every active
projector, or those in `projectors`). Only `projectors`, `samples` and `apply` go with `from`.
It writes `matrix` and `curves` as a camera run does.

How it matches: per camera channel, the reading of a projector's grey as a function of its
signal; two projectors give the same light where the camera reads the same. Each channel
follows the projector whose white is the dimmest in that channel, so when no projector is the
dimmest in every channel (one warmer, one cooler), the common white lies between their white
points and keeps more brightness than matching to either one. When one projector is the
dimmest everywhere, it keeps its picture and the others follow it, as in a camera run.

Limits: there is no closed loop, so trust it less than a camera run. Readings only compare
when they were taken with the same camera, exposure, white balance and patch of wall; files
from different runs are combined with a warning. Files without a grey ramp (from an older
format) are matched from each primary alone, which through a camera that does not add up can
leave white off by several per cent per channel. A camera run warns when its projectors would
gain from a white between the white points.

### Even light across a projector, and black in overlaps

Within one projector's picture brightness and colour vary too: brighter in the middle (a hot
spot), a colour cast towards an edge. And where projectors overlap, their blacks add up, so
black there is brighter than beside it. `projector.flatfield` measures both with the camera
and writes per projector two pictures, which `out.projector` takes at its `gain` and `black`
inputs:

    penumbra-cli ctl projector.flatfield '{"camera": "/cam", "projectors": ["/proj_a", "/proj_b"], "scans": ["scan/proj_a", "scan/proj_b"]}'

`scans` are the projectors' scan folders (from `scan.capture`, see [scanning](scanning.md)):
they tell which camera pixel each projector pixel lands on, so they must fit the current
setup. It takes about 40 seconds for two projectors. All projectors show black, then each one
white alone, then each primary at a few levels (`levels`, from `low` up to full). Per camera
pixel the readings are compared with the projector's typical pixel: at which drive does this
pixel read what the typical one reads? Only comparisons, so the camera's own curve does not
matter. A smooth surface is fitted to that over the projector's picture, since a hot spot and a
cast are smooth while the surface's own pattern is not; then everything is scaled so the
dimmest part of the picture (all but `floor`, 2 %) still reaches it at full drive. The gain is
the same at the spot where `projector.match` measures, for every projector and channel, so a
match stays valid. It costs brightness: the report's `scale` is the gain there, the share of
light the projectors keep at that spot.

The gain map (`flatfield/gain_NAME.png`) multiplies the drive per pixel and channel, in linear
light, after the colour matrix and before the tone curves: the hot spot and the cast belong to
the projector's light, which its drive scales, and the curves only bend the drive into the
projector's response. The black lift (`flatfield/black_NAME.png`) raises the drive where some
projector does not reach, by that projector's black, shared among those that do:
`drive = lift + (1 - lift) drive`. The command connects both through `tex.image_file` nodes (one undo
step; `apply: false` only writes the files). The params `flatten` (0 to 1, how much of the gain
applies) and `lift` (a factor on the black lift) are in the projector's *colour* group; `correct`
off turns them off with the rest.

Measure on a flat, evenly coloured surface (the wall before objects go up, or a white sheet in
front of them): on a relief or objects the brightness also changes with the surface's slope,
and the gain would undo that shading too. The map lives in the projector's own pixels, so it
stays valid when objects are placed later, as long as the projector, its zoom and its lens shift
stay put. Where the surface is not even, `even: "colour"` measures only the colour cast (the
balance of red, green and blue, which shading does not change) and leaves the brightness alone;
the command warns when it sees an uneven surface with the default `even: "all"`.

The camera reads black poorly (dark values sit in the toe of its curve), so the measured lift
tends to be too small. Judge it by eye with everything black: if black beside the overlap is
still darker, raise `lift` (with an ordinary camera about 2). `contrast` estimates a
black the camera cannot see at all (white over contrast). `measured_NAME.png` shows what was
measured per camera pixel before the fit, `pictures: true` keeps the camera's pictures.

## Resolution

What each picture shown on a projector needs: enough pixels that one of them lands on about one
projector pixel, measured over the mappers' and render nodes' pictures (the 95th percentile).
The table lists every node feeding a projector with its size, what it needs, and a verdict:

- **ok**: between 0.9 and 1.3 picture pixels per projector pixel.
- **too coarse** (red): fewer; the projection looks soft.
- **wasted** (blue): more; it costs GPU time without showing.
- **follows its input**: the node has no size of its own; change the node upstream.

**Optimize** gives every listed node the suggested size, as one undo step. Nodes with
`keep_size` on (under "more") are left alone: use it for a deliberate resolution, a simulation
grid or a pixel look. Params measured in pixels (a blur's size, a text's size) look different
after a size change.

**overlay** colours the mappers' and render nodes' pictures by the same verdict: green where a
pixel meets about one projector pixel, red where it is too coarse, blue where it is finer than
shown.

In a `tex.render` the overlay shows on the render's texture input (with `surface` on `texture`,
or `auto` with the texture wired). Blue on far or small surfaces is normal there, not a fault:
one texture covers near and far surfaces alike, so a texture sized for the nearest surface is
necessarily finer than needed on the far ones. Aim for green where it matters, usually the
nearest large surface. On faceted meshes (a sphere of few segments) the colour borders on
curved surfaces are jagged, because the measure steps from triangle to triangle.

Where a mapper and a render are chained (a render shown through a mapper), only the one nearer
the content draws its overlay.

## State

What feedback loops, held caches and similar nodes have built up is lost on a restart unless
**keep node state across restarts** is on (the project setting `persist_state`, off by
default). Then it is written to the project's `state/` folder every so many seconds (**every**)
and when the app closes, and read back at the next start. Locked nodes keep their outputs either
way.

- **Save now** writes it at once.
- **Restore** puts back what was saved last.
- **Start fresh** makes the nodes forget what they hold, here and in the folder (locked nodes
  keep their outputs). Starting the app with `--fresh` does the same.

To start single nodes over, select them and press **R** over the node editor (with nothing
selected, R resets every stateful node of the level shown), or use the **reset** button at the
top of a stateful node's params. A component resets the stateful nodes inside it. Locked nodes
keep their outputs, and the state folder is not touched. From the control channel:
`node.reset {paths: [...]}`. A `tex.feedback`'s `reset` param stays for holding a loop at its
initial input, for example from a binding.

## OSC

OSC input comes in through `chan.osc_in` nodes, output goes out through `chan.osc_out`; this
section shows what they are doing.

- **listen** off closes every OSC port for this run; the nodes stay.
- **this machine on the network** is the address to enter on a phone or another computer. A
  `chan.osc_in` is reachable from other machines only with its `lan` param on; otherwise only
  programs on this machine reach it.
- Per listener: its port, packets and addresses received, and what was dropped (malformed,
  too large, over the rate limit). Per sender: packets sent and failures. The last messages,
  the latest per address, are listed below.

Two ways to control a param from OSC:

- **Bind it** to the `chan.osc_in`'s channel for that address, by hand or with **learn from
  OSC** in the param's link menu: the next address that arrives with a number is bound. This
  is the better way for continuous control.
- **By address:** with `params` on, a `chan.osc_in` also sets the params its addresses name:
  `/scene/speed 0.5` sets `/scene.speed`, `/n/translate/0 0.5` one component. These values are
  live: they do not enter the undo history. File params and a script's code are never set
  this way, so whoever can send OSC cannot make penumbra load a file or run a program.

## Audio

What the [audio input](audio.md) is doing:

- **listen** off closes every audio input for this run; the nodes stay.
- Per input a `chan.audio_in` opened: the device, its sample rate and channels, glitches the
  system reported, an error if the device is missing or failed, and a level meter per channel
  (red near clipping).
- **inputs on this machine** lists the input devices with their format; **list again** after
  plugging one in. Part of a name goes into a `chan.audio_in`'s `device`.

## Show machine

Windows settings that can ruin a show, each with a **Fix** and, after fixing, a **Restore**
that puts back what the fix replaced:

- **Gaming overlay:** Windows may treat a fullscreen output as a game and show the Game Bar
  over it. The fix turns off game capture and the Game Bar for the current user.
- **Notifications:** banners for updates, mail and chat pop up on the main display. The fix
  turns notification banners off for the current user.

Not checked here, because it lives in the graphics driver: with G-Sync (or FreeSync) enabled
for windowed mode, the monitor follows the frame rate of the focused window. The editor
redraws at about 60 Hz while nothing moves, so a 240 Hz monitor drops to 60 Hz (the mouse
pointer turns choppy) as long as penumbra has the focus. Enable G-Sync for fullscreen only, or
set **Monitor Technology** to **Fixed Refresh** for `penumbra.exe` in the NVIDIA Control Panel.
The output windows are not affected; they present at their display's fixed vsync.

For the performance itself, [show mode](getting-started.md#show-mode) locks the project
against edits and starts the app again after a crash.
