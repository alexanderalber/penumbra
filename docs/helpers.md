# The Helpers window

Right-click on empty canvas and choose **Helpers**. The window collects what matters when
setting up a show rather than while building one. Everything in it is also a command (named in
the tooltips), so Claude can do the same.

## Outputs

Which displays are connected (size, refresh rate), which one shows the editor, and the output
window open on each: frames per second, missed frames, the longest frame. Below that, every
active `out.projector` with the display it is on, and its colour in the test pattern.

The show clock follows the vsync of one output window (marked with a dot), or a timer when no
window runs at the show rate. The status bar repeats this in short ("no outputs · timer").

The timer ticks as evenly as a projector's vsync, so timing while developing without a
projector is what it will be at the show. It costs a few percent of one CPU core.

The display ids listed here (`EPSON PJ @ port 4`) are what an `out.projector`'s `display`
takes; the monitor name alone is enough when it is unique. The `test` param of a projector
shows a test pattern in the projector's own pixels, in its colour, so overlapping projectors
can be told apart.

While at least one output window is open, penumbra tells the operating system that the displays
are in use, so Windows does not turn them (and the projectors) off after its idle timeout. The
Outputs section shows this as one line; `output.list` reports it as `keeps_display_awake`. The
request ends when the last output window closes or penumbra exits.

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
`drive = lift + (1 - lift) drive`. The command connects both through `tex.image` nodes (one undo
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
