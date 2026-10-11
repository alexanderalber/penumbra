# Camera

`tex.videoin` shows a live camera: a capture card such as the Elgato Cam Link behind a camera's
HDMI output, or a webcam. Tracking (blobs, laser spots) will build on it.

## Setting up

1. Add a `tex.videoin` (Tab > tex > videoin). With `device` empty it opens the first camera of
   the machine. The `...` menu beside `device` lists the cameras; part of a name typed in works
   too, e.g. `Cam Link`.
2. `mode` picks size, rate and pixel format. Empty takes the largest, fastest mode the camera
   offers. The `...` menu beside `mode` lists the modes of the camera `device` picks; a partial
   mode works too: `@30` (any size at 30 fps), `1280x720`, `YUY2`.

The node recooks when a frame arrives, not on the show clock, so the picture stays live while
the show is paused. It always shows the newest frame; frames that arrive while nothing looks at
the node are skipped, never queued up. If the camera goes away (a pulled cable), the node keeps
its last picture with a warning, and the camera is opened again every two seconds until it is
back.

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

`videoin.devices` lists cameras and modes, `videoin.status` the open streams (fps, dropped,
delay, error), `videoin.config {enabled}` switches all cameras off and on for this run. See the
[command reference](reference/commands.md).

Camera input exists on Windows (Media Foundation) so far; on other systems the node reports
that it is not built there yet.
