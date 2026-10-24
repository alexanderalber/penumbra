# python-script

Needs Python 3.12 or later (python.org). Without it, `/physics` shows an error saying what is
missing, and the ball stays in the middle.

`/physics` runs `bounce.py`: a ball bouncing in a box under gravity. The script keeps the
ball's position and speed in module globals, which last from one frame to the next, and writes
them out as channels (`x`, `y`, `tx`, `ty`, `bounces`). `/place` is bound to `tx` and `ty` and
moves the glowing ball drawn by `/ball`. The loop through `/loop`, `/fade` and `/echo` leaves a
fading trail behind it (see the feedback example), which traces the arcs the script computes;
`/view` lays it over a dark gradient, `/backdrop`.

Try:

- `gravity` and `bounce` in `/physics`' params (declared by the script's `PARAMS`).
- `opacity` in `/fade` closer to 1 for a longer trail.
- Edit `bounce.py` in any editor and save: the node reloads it.

See `docs/scripts.md`.
