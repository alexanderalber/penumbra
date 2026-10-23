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
  target, roll and field of view (an orthographic camera's `size` instead), as one undo step.
- **steer**: the render's own output again, but navigating moves its camera. Each movement is
  one undo step: a drag, a turn of the wheel, or a push of the 3D mouse until the cap rests.

The camera that moves is the `view.camera` wired into the render's `camera` input; a render
needs one, and without it the render, steering and `render.frame` say so. A calibrated camera (one whose `calibration`
param holds a `calib.solve` result) cannot be steered, because that would replace the
calibration. The numbers in the param panel stay the precise way to place a camera.

## Orthographic cameras

A `view.camera` with `projection` on `orthographic` sees with parallel rays: things keep their
size at any distance, as in a plan or elevation drawing. `size` says how much the picture
shows from bottom to top, in metres; the width follows the picture's aspect. `fov` does not
count then (its row is greyed), and in perspective `size` does not. Panning moves by picture
heights as before; coming closer or moving away changes `size`, since the distance changes
nothing in the picture. The camera still has a position: nothing behind it (nearer than
`near`) is drawn. A calibration (`calibration`) is always perspective and wins over
`projection`. A `light.projector` with an orthographic camera throws its picture in parallel,
like a directional light inside the camera's box, and the free view draws such a camera as a
box instead of a pyramid.

## Mouse

| Input | What it does |
|---|---|
| Left drag | Turns about the point looked at. The horizon stays level. |
| Middle drag, or Shift and left drag | Moves sideways; the scene follows the pointer. |
| Right drag, mouse wheel | Comes closer or moves away; an orthographic camera sees more or less instead (its `size`). |
| Double-click | Starts the free view over. |

## 3D mouse

penumbra reads a 3Dconnexion SpaceNavigator directly, **without** the 3DxWare driver. The
driver claims the device for itself; if it is installed, penumbra cannot open the device and
`spacenav.status` says so. Plugging in, unplugging and plugging in again work while the app
runs.

The 3D mouse flies the viewer that shows a free view or steers (the main viewer first, else
the viewer's window). To steer a show with it instead, `chan.spacemouse_in` gives its axes and
buttons as channels ([concepts](concepts.md), "Mouse and 3D mouse"); both follow the cap at
once. It flies like a camera operator:

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
- `render.frame` moves the render's camera so its geometry fills the picture, keeping the
  direction; an orthographic camera gets the `size` that shows it all.
- `view.free` draws a node's free view from any camera as a PNG, without the UI.
- `spacenav.status` shows the 3D mouse's state, its raw counts and the shaped axes.
