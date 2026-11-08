# Screen and window capture

`tex.screen_in` turns a monitor or a window of this machine into a picture. Use it to put a
browser, a slide deck, a video call or another program's output into a mapping, or to show the
desktop on a projector. The picture stays on the graphics card, at the size of what is captured.

Screen capture is Windows only (Windows 10 1903 or later). For a program that speaks Spout,
[Spout](spout.md) is the better way: its picture has no window frame and no pointer, and it
keeps transparency.

## Capturing: `tex.screen_in`

Add a `tex.screen_in` (Tab > tex > screen_in). With the defaults it shows the primary monitor.

- `source`: `monitor` or `window`.
- `display`: which monitor. Empty: the primary one. A number: the n-th monitor, in the order of
  their OS names (`\\.\DISPLAY1` first). Or a display id, as `out.projector` takes
  it. The **...** menu lists the monitors with their number, id and size.
- `window`: a part of the window's title, upper or lower case alike, such as `notepad` or
  `firefox`. The frontmost window that matches is captured. The **...** menu lists the windows
  open now, front to back, with their size and program.
- `cursor`: draw the mouse pointer into the picture. Off by default.
- `border`: let Windows draw its yellow frame around what is captured. Off by default; Windows
  10 draws it anyway.

The picture follows the window when it is resized. A minimized window sends nothing, so the node
keeps its last picture. Windows sends a new frame only when something changed on the monitor or
in the window, at most at the monitor's refresh rate. The picture stays live while the show is
paused, like a camera's.

`screen.sources` on the control channel lists the monitors and windows as the menus do.

## When the window or monitor goes

When the window closes or the monitor is unplugged, the node keeps showing the last picture and
warns. It looks for the window or monitor again, every quarter second at first and every 4 seconds
later. A window that comes back with a matching title is captured again; it does not have to be
the same window. Before the first picture the node shows an error instead: no window matches, or
Windows cannot capture here (some remote desktop sessions cannot).

## A web page as a picture

Open the page in a browser window and capture that window by a part of its title. Hide the
browser's toolbars (F11 or a kiosk mode) if only the page should show.

## Colour and cost

The desktop is captured as 8-bit sRGB and converted to penumbra's linear light, opaque. HDR
monitors were not tried. Each frame costs two copies and one conversion pass on the graphics
card; nothing goes through the CPU.
