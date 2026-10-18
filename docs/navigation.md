# Looking around in 3D

When the viewer shows geometry or a `tex.render`, you can look at the scene with a camera of the
viewer's own: the **free view**. It changes no output and is not in the undo history.

## The viewer's modes

A geometry node always shows the free view. It starts framed as the node's thumbnail.

A `tex.render` gets a line of buttons under the viewer's header:

- **picture**: the render's own output (the default).
- **free**: the scene from the viewer's camera. The render's camera is drawn into it in silver.
  Projector lights are drawn as yellow frusta, spot lights as cones, point lights as small stars
  and directional lights as arrows. The free view starts where the render's camera is.
  **take view** moves the render's camera to where the free view looks from: position,
  target, roll and field of view, as one undo step.
- **steer**: the render's own output again, but navigating moves its camera. Each movement is
  one undo step: a drag, a turn of the wheel, or a push of the 3D mouse until the cap rests.

The camera that moves is the `view.camera` wired into the render's `camera` input, or the
render's own camera params when nothing is wired. A calibrated camera (one whose `calibration`
param holds a `calib.solve` result) cannot be steered, because that would replace the
calibration. The numbers in the param panel stay the precise way to place a camera.

## Mouse

| Input | What it does |
|---|---|
| Left drag | Turns about the point looked at. The horizon stays level. |
| Middle drag, or Shift and left drag | Moves sideways; the scene follows the pointer. |
| Right drag, mouse wheel | Comes closer or moves away. |
| Double-click | Starts the free view over. |

## 3D mouse

penumbra reads a 3Dconnexion SpaceNavigator directly, **without** the 3DxWare driver. The
driver claims the device for itself; if it is installed, penumbra cannot open the device and
`spacenav.status` says so. Plugging in, unplugging and plugging in again work while the app
runs.

The 3D mouse flies the viewer that shows a free view or steers (the main viewer first, else
the viewer's window). It flies like a camera operator:

| Cap | Camera |
|---|---|
| Push away, pull towards you | Forward, back |
| Slide sideways | Sideways |
| Lift, press | Up, down |
| Tilt forward or back | Look up or down |
| Twist | Turn left or right |
| Tilt sideways | Roll, only while the horizon is unlocked |

The **left button** locks and unlocks the horizon. It starts locked, so flying never tilts the
picture by accident. The **right button** starts the free view over. The speed follows the
distance to the point looked at: far away it flies fast, close up slowly.

## Commands

- `ui.camera` does all of the above from the control channel: switch the mode, set the free
  view's camera, move it (`orbit`, `pan`, `dolly`, `travel`, `look`, `roll`), `take` the view.
- `view.free` draws a node's free view from any camera as a PNG, without the UI.
- `spacenav.status` shows the 3D mouse's state, its raw counts and the shaped axes.
