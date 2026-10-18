# Camera

`tex.video_in` shows a live camera: a capture card such as the Elgato Cam Link behind a camera's
HDMI output, or a webcam. Tracking (blobs, laser spots) will build on it.

## Setting up

1. Add a `tex.video_in` (Tab > tex > video_in). With `device` empty it opens the first camera of
   the machine. The `...` menu beside `device` lists the cameras; part of a name typed in works
   too, e.g. `Cam Link`.
2. `mode` picks size, rate and pixel format. Empty (`auto` in the menu) takes the largest size
   the camera offers at its highest rate. The `...` menu beside `mode` lists the modes of the
   camera `device` picks; a partial mode works too: `@30` (any size at 30 fps), `1280x720`,
   `YUY2`.

The node recooks when a frame arrives, not on the show clock, so the picture stays live while
the show is paused. It always shows the newest frame; frames that arrive while nothing looks at
the node are skipped, never queued up. If the camera goes away (a pulled cable), the node keeps
its last picture with a warning, and the camera is opened again every two seconds until it is
back.

A frame identical to the one before, byte for byte, is skipped. A camera sending 30p over a
60 Hz HDMI signal (common: a Cam Link then reports 59.94 fps) repeats every frame, and the
repeats would make motion and averaging see a still picture every other frame. Such a camera
gives about 30 new pictures a second; set the camera's HDMI output to 60p for 60.

A camera can be open in one program at a time on most systems: if another program (OBS, the
camera app, a browser) holds it, the node reports that the camera cannot be opened.

## Modes and colours

Only modes with 8-bit Y'CbCr pixels are read: NV12, I420, YUY2, UYVY. The menu lists the others
(MJPEG, RGB, 10-bit) greyed out. Frames are converted on the GPU, as video files are; the
camera's rearranging costs the capture thread a few milliseconds per 1080p frame.

A camera set to "60 fps" usually sends 59.94 (60000/1001); `@60` matches it. A Cam Link 4K fed
1080p60 offers exactly that size and rate, as NV12, I420 or YUY2: the camera's HDMI output
decides, not penumbra.

`range` (under "more") says whether black is code 16 (limited, normal for video) or 0 (full).
`auto` believes the camera and assumes limited when it does not say. Washed-out blacks and
dull whites mean limited is assumed for a full-range signal; set `range` to `full`. Crushed
blacks mean the opposite.

## The `info` output

| channel | what |
|---|---|
| `fps` | frames per second arriving, measured over two seconds |
| `delay_ms` | from the camera driver's timestamp to penumbra having the frame |
| `age_ms` | from penumbra having the frame to this cook |
| `frames` | frames received since the camera was opened |
| `dropped` | frames lost before they reached penumbra, judged by gaps in the camera's timestamps |

`delay_ms` and `age_ms` cover only the part inside the computer. The camera's own processing,
its HDMI output and the capture card come before the driver's timestamp and are not in them;
measuring the whole path from light to projector needs the projector filmed by the camera.

## Commands

`video_in.devices` lists cameras and modes, `video_in.status` the open streams (fps, dropped,
delay, error), `video_in.config {enabled}` switches all cameras off and on for this run. See the
[command reference](reference/commands.md).

Camera input exists on Windows (Media Foundation) so far; on other systems the node reports
that it is not built there yet.

## Noise

A camera in a dark room is noisy, and the noise flickers into whatever reads the picture
(tracking, a feedback, a projection of the camera itself). `tex.denoise` behind the
`tex.video_in` calms it: where the picture is still it averages over about `frames` frames;
where it moves (a change beyond `motion`) it shows the new frame at once and smooths that
within the frame instead (`spatial`), keeping edges. Raise `motion` when the noise still
flickers; lower it, or lower `frames`, when moving things leave trails. It costs about half a
millisecond per 1080p frame.

Brightness noise (grain) and colour noise (coloured speckles) have their own strengths:
`frames` and `spatial` for the brightness, `color_frames` and `color_spatial` for the colour.
Colour can usually take more than brightness before the picture looks soft. `frames` is not a
count of frames kept but how fast the average follows (each new frame weighs `1 / frames`), so
fractions such as 2.5 are meaningful, and a channel can drive it smoothly.

## Motion

`tex.optical_flow` behind the `tex.video_in` (or behind a `tex.denoise`) measures how far each
part of the picture moved since the previous frame. Its output is a small picture, `size` wide
(320 by default, under "more"), whose red and green are the motion in image sizes per frame:
red to the right, green downwards (x in widths, y in heights), negative the other way; blue is 0,
so a still picture is black. With `confidence` (under "more") blue says instead how well the
motion there is pinned down (0 to 1): a plain wall cannot show which way it moves, an edge only
across itself.

The numbers are not for looking at; turn on `colors` to see the motion as colours, direction as
hue and speed as brightness (full at 1/80 of the width per frame). `threshold` fades out motion
slower than that, in pixels at the analysed size per frame, so camera noise does not shimmer;
a still camera picture moves by 0.05 to 0.15 of those pixels by noise alone.

Motion can only be measured where the picture has something to follow: edges and texture. On
plain parts (skin, a shirt, a wall) the raw measurement is unsure and noisy. `fill` spreads the
motion found at the edges into the plain parts of the same thing, not across edges into the
background; each round reaches twice as far (4 rounds: about 30 pixels at the analysed size).
More rounds make a large plain thing move as one; 0 shows the raw measurement.

Where a pixel's content was in the previous frame is its position minus the motion, so a
`tex.displace` with `midpoint` 0 and `weight` -1, -1 pulls the previous frame onto the current
one, and a positive weight pushes the picture along with the motion.

Under "more": `levels` (each doubles the fastest motion it follows), `iterations` (refining steps per level) and
`window` (the neighbourhood each pixel is judged from: larger is smoother, smaller keeps small
moving things apart). With the defaults it costs about 3 ms per new frame on the CPU, most of it
handing its passes (two per level, one per iteration and level, one per `fill` round) to the GPU; the GPU's part
is small.
