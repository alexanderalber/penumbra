# channels-and-pictures

Pictures and channels turn into each other. The picture they start from, `/picture`, is grey
noise (`/noise`) coloured through the colormap `plasma` (a `tex.lookup` with a `palette`).

- `chan.from_tex` reads a picture into channels: `/brightness` the brightness of 128 columns,
  `/columns` the colour of 64 columns, `/mean` the picture's mean colour.
- `tex.from_chan` turns channels back into a picture: `/curve` and `/strip` are one pixel high
  and as wide as the channels have samples. Its `layout` `auto` sees that `/columns` holds the
  channels `r`, `g`, `b` and makes `/strip` a colour row; `/curve` (one channel, `lum`) is grey.
  A `tex.from_chan` after `/mean` would be a 1 x 1 swatch of the mean colour.
- `/swatch` is bound to `/mean`, so it shows the mean colour.
- `/view` (a shader, `view.wgsl`) draws it all over the picture: the brightness as a curve,
  the column colours as a strip at the bottom, the mean colour as a square at the top right.
  The curve is drawn around the mean brightness, stretched by `gain`.

Try:

- Another `palette` in `/picture` (`sea`, `twilight`, `candy`): the strip and the square follow.
