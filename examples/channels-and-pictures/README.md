# channels-and-pictures

Pictures and channels turn into each other:

- `chan.from_tex` reads a picture into channels: `/brightness` the brightness of 128 columns,
  `/columns` the colour of 64 columns, `/mean` the picture's mean colour.
- `tex.from_chan` turns channels back into a picture: `/curve` and `/strip` are one pixel high
  and as wide as the channels have samples.
- `/swatch` is bound to `/mean`, so it shows the mean colour.
- `/view` (a shader, `view.wgsl`) draws it all over the picture: the brightness as a curve,
  the column colours as a strip at the bottom, the mean colour as a square at the top right.
