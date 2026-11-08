# python-pictures

Needs Python 3.12 or later (python.org) with numpy (`python -m pip install numpy`). Without
numpy, `/rings` and `/sheet` show an error saying what is missing.

`/rings` runs `rings.py`: it draws a picture of 256 x 256 pixels with numpy, coloured rings
around two centres that circle each other. The script sets `cx.picture`, which comes out of the
node's `picture` output. `/sheet` runs `sheet.py`: it builds a mesh of 80 x 80 vertices with
numpy, a sheet with waves running over it. The script sets `cx.mesh`, which comes out of the
`mesh` output. `/shine` takes the rings as its texture, `/skin` puts it on the sheet, and
`/render` lights the sheet with `/light`, seen through `/camera`.

Try:

- `rings` and `speed` in `/rings`' params, `height` and `waves` in `/sheet`'s (declared by the
  scripts' `PARAMS`).
- Edit `rings.py` or `sheet.py` in any editor and save: the node reloads it.

See `docs/scripts.md`, section "Pictures and meshes".
