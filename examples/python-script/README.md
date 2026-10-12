# python-script

Needs Python 3.12 or later (python.org). Without it, `/physics` shows an error saying what is
missing, and the ball stays in the middle.

`/physics` runs `bounce.py`: a ball bouncing in a box under gravity. The script keeps the
ball's position and speed in module globals, which last from one frame to the next, and writes
them out as channels (`x`, `y`, `tx`, `ty`, `bounces`). `/place` is bound to `tx` and `ty` and
moves the ball drawn by `/ball`.

Try:

- `gravity` and `bounce` in `/physics`' params (declared by the script's `PARAMS`).
- Edit `bounce.py` in any editor and save: the node reloads it.

See `docs/scripts.md`.
