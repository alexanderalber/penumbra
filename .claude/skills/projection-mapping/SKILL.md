---
name: projection-mapping
description: "Map projectors onto real objects with penumbra and a camera: check the hardware, scan every projector with structured light, find the faces and objects, blend the projectors, put content on the surfaces and look at the result. Use when someone wants to project onto objects, a relief, boxes, a pumpkin or a room with penumbra, when a scan has to be run or redone, or when mapped content lands in the wrong place."
---

# Projection mapping with penumbra

penumbra drives the projectors and the camera; everything below is a penumbra command sent
over its control channel with `penumbra-cli ctl <method> '<json>'` (`--port N` before `ctl`
when the app does not listen on 9880). The reply is JSON; `"ok": false` carries the error.
`penumbra-cli ctl commands` lists every command of the running app; the full guide to this
chain is `docs/scanning.md` in the penumbra repository.

The chain: hardware check, scan, faces and objects, blend, content, look. Each step has a
check; do not go on past a failed one, fix it or scan again.

## Ground rules

- **Nothing moves.** Camera, projectors and objects stay put from the scan to the show. A
  bumped tripod or a projector that creeps on its stand makes every map wrong by that much.
  Projectors standing on soft supports (crate lids, cardboard) creep for hours after being
  moved: scan again before judging alignment.
- **Look at every result.** The commands write PNGs; open them (`quality.png`,
  `scene_view.png`, `regions_view.png`, snapshots) and judge them before the next step. A
  reply that says `ok` is not a picture that looks right.
- **One command at a time**, read its reply (warnings included) before the next.
- **Never kill the app in the middle of a scan.** `scan.cancel` stops a `scan.run`;
  `scan.capture` ends by itself. To restart the app, stop the process when no command runs.
- Paths in commands are relative to the open project's directory; absolute paths work too.

## 0. The app

- `penumbra-cli` with no arguments pings the app. No answer: start it, `penumbra.exe
  --project <dir> --port <port>` (without `--project` it opens the last project).
- `project.info`: which project is open, nodes that failed to load.
- The project needs one `out.projector` per projector, its `display` set from `output.list`,
  and a `tex.video_in` for the camera (`node.create` with `kind`; `node.list` shows what is
  there).

## 1. Hardware check

Before every scan:

1. `output.list`: every projector is a connected display and its `out.projector` names it.
   Display ids can change when displays are plugged in or out: compare names, not numbers.
2. The camera delivers: `node.snapshot {"path": "/cam", "file": "check_cam.png"}`, open it.
   A black or frozen picture (two snapshots a few seconds apart identical, noise included)
   means no signal.
3. **Room dark:** with every projector black, the camera's picture should be near black
   (mean well under 10 of 255). Room light washes out the patterns. Ask the person to switch
   lights off and close curtains if it is not.
4. **Camera locked:** exposure, white balance and focus manual, and no automatic dynamic
   range (Sony DRO, auto HDR): these cannot be read over HDMI, so ask the person to confirm
   them. A camera that adjusts itself between patterns gives garbage decodes.
5. **Not clipping:** a projector at full white on the scene should not saturate the camera.
   If it does, lower the exposure on the camera, or scan with two levels (below).
6. Projectors in focus: a sharp pattern edge in the camera picture; ask the person to
   refocus when it is soft.

## 2. Scan

One projector at a time, into a fresh folder per scan (never overwrite the last good one):

    penumbra-cli ctl scan.capture '{"camera": "/cam", "projectors": ["/proj_a"], "out": "scans/scan1", "levels": [1, 0.5], "frames": true, "options": {"min_contrast": 0.01}}'

then the same for every further projector. Each takes 45 to 60 s at full HD with two levels.
`levels: [1, 0.5]` scans twice and merges (the dim pass fills where the bright one clipped);
list the bright level first. `frames: true` keeps the camera pictures, so a scan can be decoded
again later without the hardware. `min_contrast` 0.01 lets dark faces decode at the dim level.

Check each projector's reply and files:

| look at | good | bad, and what to do |
|---|---|---|
| decoded camera pixels | a large share of where the projector visibly lands | far fewer than last time: something moved, the room got brighter, or the projector was dimmed; check the hardware again |
| `resolved_bits` of `bits` | 9 to 10 of 11 is normal for a full HD camera (the finest stripes are finer than it resolves) | 7 or fewer: out of focus, camera too far, or exposure wrong |
| `quality.png` | grey where the projector lands | magenta: clipped, lower exposure or add a dim level; blue: ambiguous; red: outliers; black on a surface the projector lights: too dark, add `min_contrast` 0.01 or a brighter level |
| `off_geometry` | small (under a few percent) | large: light bounced off nearby light walls was decoded; the epipolar check drops it, but if most of the decode is off, rearrange or darken the walls |
| warnings | read them all | a frame that never settled, a pair that had to be retaken: scan again |

Camera stills of the scene, for previews and for content drawn from photos: every projector
black, `node.snapshot` of the camera into the scan folder (`dark.png`); then each projector
alone lit white (connect a constant white or grey into its `out.projector` with
`node.connect`), a snapshot each (`photo_a.png`, ...); everything back to black. The camera
picture lags the projector by a second or two: wait before each snapshot.

**Drift against the previous scan.** When an earlier scan of the same setup exists, compare
the two `cam_to_proj.exr` maps of each projector over camera pixels decoded in both (red and
green are the projector position, 0 to 1; scale by the projector's size): the median shift in
projector pixels is how far that projector moved against the camera. Under 1 to 2 px is creep;
content built on the old scan is off by that much. More: something was bumped, rebuild
everything from the new scan.

## 3. Faces and objects

Pick by the scene:

- **A scene of flat faces with objects among them** (boxes, crates, a wall, a floor, a pumpkin
  on a crate): `scan.scene {"scans": ["scans/scan1/proj_a", "scans/scan1/proj_b"], "out": "scene"}`.
  No reference scan needed.
- **One mostly flat object** filling the frame (a relief panel): `scan.relief`.
- **Objects in front of an empty wall** with a scan of the empty wall as reference:
  `scan.objects`.

Open `scene_view.png` (faces in flat colours, objects white, rectified faces with a grid of true
squares). Typical failures:

| what shows | why | fix |
|---|---|---|
| a lid or top merged into the object standing on it | the face sags or is seen at a grazing angle and fits no plane | give its four corners in camera pixels: `rectify: [{"aspect": [60, 40], "corners": [[x,y], ...]}]` (far left corner first) |
| the wrong face is the support | the face below the object was not found or another wins | `support` names the face, or `"support": "rectified"` with the corners above |
| heights inverted (valleys bright) | the sign heuristic guessed wrong | run again with `sign` the opposite of the reply's `sign.as_param` |
| a small stray object seen by one projector only | noise or bounced light | ignore if content does not use it, or mask it |
| one wall with two numbers | two projectors light halves of it without overlap | expected; group them with `scan.regions` |

For content per part of the scene (each box front, each lid), group the faces with
`scan.regions {"scene": "scene", "edit": [...]}` by camera pixels, and open
`regions_view.png`. The floor is off by default. The grouping lives in `regions.json` and
survives a rescan while camera and objects stay.

## 4. Blend

    penumbra-cli ctl scan.blend '{"scans": ["scans/scan1/proj_a", "scans/scan1/proj_b"], "masks": ["scene/labels_0.png", "scene/labels_1.png"], "out": "scene"}'

writes `weight_N.png` (multiply each projector's picture by it) and `proj_to_cam_smooth_N.exr`
(the map content goes through). Masks are optional; without them the whole frame counts. The
reply says how closely the weights add up to 1 where projectors overlap.

## 5. Content

Content is drawn once, in the camera's picture, and each projector takes its share:
`tex.remap` of the content through `proj_to_cam_smooth_N.exr` (`extend` zero), multiplied by
`weight_N.png`, into that projector's `out.projector`. Outlines of faces and objects come from
`tex.shapes` tracing `objects_camera.png` or `regions_camera.png` (`shapes.trace` with `live`).
`docs/scanning.md`, "The network", has the node chain.

If the project already holds prepared content with its own rebuild step (a script that takes
a scan folder and rewrites the project), run that step on the new scan instead of building
content by hand, and do not change the content itself.

Content drawn in the camera's picture is right from the scan camera's viewpoint. Seen from
anywhere else (a second camera filming the setup, the audience), only content attached to the
surfaces looks right: fills, outlines, glows and textures in a face's own coordinates (uv of a
rectified face). A flat picture laid over the camera image looks like a sticker seen at an
angle.

## 6. Look

`node.snapshot` of each projector's final picture and of the camera with the content running,
and open them: content on the objects and not on the wall behind, no double-bright band where
projectors overlap, no seam. Then ask the person to look at the real thing: the camera does
not see everything (the wall behind an object, edges past its view).

## When to scan again

- Something moved (drift above 2 px, or content visibly off on the real objects).
- The decode dropped clearly against the last scan of the same setup.
- `quality.png` shows a lot of magenta (clipping) or black on faces that should decode.
- Lights changed in the room, or the camera's exposure was touched.

Scan into a new folder, compare, and switch to it only when it is better.
