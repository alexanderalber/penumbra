## Lettering you can fly around

`geo.text` writes text as solid 3D letters, in any font `tex.text` can use. The letters stand upright
and face -y, so a camera at -y looking towards +y reads them. `align` places the block of text along
x. The block's middle sits at z = 0 and its thickness is centred on y = 0.

**A black body with lit edges.** Give `out` a `mat.wireframe` with `edges` on `polygons` and `faces`
on `filled`. Lines then run along the outlines of the front and the back and along sharp corners
of the sides. The triangles and the steps of curves get no line. A rounded side shows no line along
its depth, also where it turns away from the camera.

**A lit front on a dark body.** Give `out` a `mat.occluder` with `shows` on `black`, and put a
material on `front` with `geo.assign_material`. Move `front` a little towards -y with a
`geo.transform`, so the two faces do not fight. Texture coordinates span the block of text: a
picture on `front` spreads over all its letters.

**Limits.** Text is cut off after 4096 characters and at about a million triangles, with a warning.
`size`, `depth` and `spacing` outside their ranges are kept inside them, with a warning. A group of
touching glyphs with more than 8192 segments, or an outline with a broken coordinate, is left out,
also with a warning. Outlines that overlap or cross themselves are joined the way a 2D renderer
fills them, so an overlap draws no line inside a letter.
