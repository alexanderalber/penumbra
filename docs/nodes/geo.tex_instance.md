## Copies from pictures

Draws one copy of the input geometry per texel of the `position` picture, entirely on the GPU, so
tens of thousands of copies stay cheap and can move every frame. Copy `i` reads texel
`(i % width, i / width)` of each picture:

| input | what its rgb does to the copy |
|---|---|
| `position` | moves it (x, y, z in metres) |
| `scale` | scales it per axis |
| `aim` | turns its +y towards that direction, keeping +z up |
| `color` | multiplies its colour |

The pictures are read as numbers, not as colours to look at, and values above 1 are kept: make
them with a `tex.shader`, or from channels with `tex.from_chan`, which writes a row per channel and
a pixel per sample.

## The extent box

Since the positions never leave the GPU, penumbra cannot see where the copies are. `extent` is half
the side of a box around the origin that holds them all; framing a view and the shadow maps of
lights use that box. Copies outside it may lose their shadows or fall outside a framed view, so set
it to the size of what the copies fill.

For a few copies placed by numbers (a table of positions), `geo.instance` is the simpler node.
