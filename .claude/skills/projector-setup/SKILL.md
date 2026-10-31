---
name: projector-setup
description: "Set up several projectors as one picture with penumbra and a camera: connect and tell the projectors apart, align them with test patterns, set the camera's exposure, match their colours, choose the overlaps, scan, blend and even out the light, then check the seams. Use when someone sets up two or more projectors, wants them to look alike, sees a seam or a colour step between projectors, gets clipping or white-boost warnings from projector.match, or wants measurements kept per lamp mode."
---

# Several projectors as one picture

Every step below is a penumbra command over the control channel:
`penumbra-cli ctl <method> '<json>'` (`--port N` before `ctl` when the app does not listen on
9880). The reply is JSON; `"ok": false` carries the error. For objects in the scene (boxes, a
relief) the `projection-mapping` skill takes over after step 5.

**On Windows**, send the JSON from a POSIX shell (Git Bash). Windows PowerShell 5.1 breaks the
quotes of JSON passed to a program, and the command then fails or does something else.

## Ground rules

- **One change at a time.** Read each reply, warnings included, before the next command.
- **Look at the camera.** `node.snapshot {"path": "/cam", "file": "check.png"}` and open the
  picture. A reply with `ok` is not a picture that looks right.
- **Long commands run as jobs.** `projector.match`, `projector.flatfield` and `scan.blend` take
  `wait: false`. Then poll `job.status` every 10 to 15 s: while the job runs, its reply has
  `running`; when `running` is missing, the result is under `last`.
- **Ask the person** for what the camera cannot show: lamp mode, picture mode, the camera's
  settings, whether someone walks through the picture.

## 1. Connect and tell the projectors apart

1. `output.list` lists the connected displays. Each id names the model and the connector, for
   example `EPSON PJ [SECA519] @ target 4352`. Two projectors of the same model differ only by
   the connector part, so a swapped cable swaps them.
2. One `out.projector` per projector, `display` set to its id:
   `node.create {"path": "/proj_a", "kind": "out.projector", "params": {"display": "..."}}`.
3. `projector.test {"on": true, "pattern": "align"}` draws a test pattern on every projector.
   With several, each one draws in its own colour (three: red, green, blue in node order). Ask
   which colour stands where and rename the nodes to match (`node.rename`).
4. Patterns for the person at the projectors: `align` (cells, centre cross, circle, edge) for
   keystone and position, `focus` (one-pixel checkers) for sharpness, `motion` (moving bars and
   a frame counter) for stutter. `{"on": false}` switches back to the input.

**Check:** `output.list` shows every projector with an open window at the display's rate and
`missed` near 0, and `pace` is `vsync`.

## 2. The camera

The camera needs manual exposure, white balance and focus, and no automatic dynamic range
(Sony DRO, auto HDR). These settings cannot be read over HDMI, so ask the person to confirm
them. Add it with `node.create {"path": "/cam", "kind": "tex.video_in", "params": {"device":
"<part of its name>"}}`; `video_in.devices` lists the cameras.

**Exposure decides the colour match.** A camera that clips white makes `projector.match` finish
with a correction that is wrong in every channel. Check before the first long run:

    penumbra-cli ctl camera.exposure '{"camera": "/cam"}'

It takes a few seconds. Each projector alone shows white, and the camera is read where that
projector lights the picture. Give `area` as for the match to read only the measuring spot.

1. Look at `peak` per projector: the brightest pixels, of 255. The brightest projector should
   reach about 200, and `clipped_percent` must be 0. `black` should stay under 10.
2. `verdict` `clipped`: close the aperture or lower ISO, then check again. `dark`: open up or
   raise ISO. `stops` says by how much (a rough guide, since cameras do not encode like sRGB).
3. `verdict` `unsteady`: the light changes between frames (`flicker_percent`) or bands move
   through the rows (`banding_percent`). Set the shutter to the projectors' frame time (1/60 s
   for 60 Hz) or longer. Some projectors hold their picture steady even at 1/1000 s; others,
   DLP in particular, do not.
4. From here on nobody touches the camera's settings.

`projector.match` and `projector.flatfield` run the clipping part of this check on their own
first pictures. They stop within seconds when the camera clips, instead of finishing a wrong
measurement.

## 3. Colour match

The projectors must overlap somewhere the camera sees. Measure where all of them overlap:
give that place as `area` ([x, y, w, h] of the camera picture, 0 to 1).

    penumbra-cli ctl projector.match '{"camera": "/cam", "projectors": ["/proj_a", "/proj_b", "/proj_c"], "area": [0.46, 0.34, 0.06, 0.33], "save": "match", "note": "lamp mode, picture mode, camera settings", "wait": false}'

It takes about 30 s per projector. Nobody may walk through the picture meanwhile. The dimmest
projector becomes the reference; the others are dimmed to it.

| warning | what it means | what to do |
|---|---|---|
| stopped before measuring: the camera clips the white of ... | nothing was measured | lower the exposure, `camera.exposure` again, run again |
| the camera clips its white | the measurement is wrong | lower the exposure, `camera.exposure` again, run again |
| white is 0.7 .. 0.9 times red + green + blue | partly the camera's own curve | normal for most cameras; below about 0.6 look for a white boost or dynamic contrast in the projector's menu |
| no projector is the dimmest in every channel: about N % more brightness | a white between the white points would be brighter | with N over 20 %, match again `from` the saved files |
| cannot show the green of white as bright as the reference | one projector cannot reach the reference's white | lower the reference's `gain` a little |
| the projectors do not overlap in the camera's view | each was measured on its own part of the wall | check the seams afterwards (step 6) |

**Measurements per lamp mode.** `save` writes one `NAME.json` per projector. Measure each mode
the person might use (Eco, Normal) into its own folder with the same camera settings. Any mix
then works without a camera: `projector.match {"from": {"/proj_a": "match-normal/proj_a.json",
"/proj_b": "match/proj_b.json"}}`. The reply's `saved.stored_match_against_this` says how far a
match from the files lands from the camera run: under 0.03 is close.

## 4. Where the projectors stand

Where two projectors overlap, their blacks add up, and LCD and DLP projectors show no true
black. The flat field (step 5) lifts the black everywhere to the brightest overlap. That costs
contrast in the whole picture.

- Overlap neighbours by 10 to 20 % of a picture's width; that is enough for a soft seam.
- Never let three projectors light the same spot, unless the extra brightness there is wanted.
- The colour match does not depend on where the projectors stand: match while all of them
  overlap, then move them apart. Moving one closer to the wall or zooming changes its
  brightness, so match again afterwards, or check the seams (step 6).
- Every projector's picture must lie inside the camera's view, with a margin.

## 5. Scan, blend, flat field

1. One `scan.capture` per projector into one folder:
   `{"camera": "/cam", "projectors": ["/proj_a"], "out": "scans/scan1", "levels": [1, 0.5],
   "frames": true}`. About 40 s each. Good: `resolved_bits` 9 or 10 of 11, `clipped` 0,
   `quality.png` grey where the projector lands. Blue on a shiny floor is a reflection and does
   no harm.
2. `scan.blend {"scans": ["scans/scan1/proj_a", ...], "out": "blend"}`. Good: the weights sum to
   1 within about 0.01.
3. `projector.flatfield {"camera": "/cam", "projectors": [...], "scans": [...], "even": "all"}`
   on a flat, evenly coloured wall (`"colour"` on a relief). It connects a gain and a black map
   to each `out.projector`. `scale` is the share of light kept, for example 0.7 for three
   projectors on a white wall.
4. Content per projector: `tex.remap` of the content (drawn in the camera's picture) through a
   `tex.image_file` of `blend/proj_to_cam_smooth_N.exr`, `extend` zero; then
   `tex.blend_weight` with `blend/weight_N.png`; into the `out.projector`.

## 6. Check the seams

**Colour.** In each overlap, light a mid grey (`tex.constant` 0.214, sRGB 0.5) and then white
through the corrected chain on one projector at a time, the others black. Compare both
projectors' camera readings over the same pixels of the overlap. Within 2 to 3 codes of 255 is
good. A steady difference in one channel against both neighbours belongs to the projector in
the middle: scale that row of its `matrix` (for example red +10 %) and compare again.

**Geometry.** A small hard white dot (`tex.primitive`, ellipse, `softness` 0, size 0.04) held
in each overlap: round and sharp, no double edge. Then let it travel across all projectors
(`chan.lfo` triangle bound to its `center` with `node.bind`). Record the camera with an
`out.video` and look at single frames. Only one projector paces the show, so a moving hard edge
can show a one-frame offset between two projectors now and then. A few pixels in rare frames is
normal; a double dot in most frames is not.

**Show machine.** `system.check` lists settings that can ruin a show. On Windows, also unplug
each projector once and choose "Extend" with Win+P. Windows stores the arrangement per set of
connected displays, and an unknown set may come up as "Duplicate", which darkens every
projector when one cable comes loose.
