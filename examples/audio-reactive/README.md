# audio-reactive

Needs a microphone or another audio input; without one the picture only drifts. `/mic` reads
the system's default input (set `device` to pick another; Helpers > Audio lists them and shows
the levels).

`/analysis` turns the sound into channels: `level`, `low`, `mid`, `high`, `kick`, `beat`,
`bar` and more (see `docs/audio.md`). The chan nodes below it shape them, and the tex chain on
the top row is bound to the results (the dashed green lines). That chain starts with grey smoke
from `/cloud`, which `/colour` (a `tex.lookup` with the palette `ice`) turns into a glow out of
black:

- `/smooth` calms the levels; `/zoom` adds 1, so the bass scales the picture in `/pump` and the
  highs raise the saturation in `/tint`.
- `/kick_fade` turns each kick into a short flash that fades out; `/punch` adds a base
  brightness for `/bright`.
- `/turn` turns the bar phase (0 to 1 over a bar) into degrees of hue for `/tint`, so the
  colours go round once per bar once a tempo is found.

`/meters` (below, on its own) takes the analysis' third output, `meters`: its meters as a
picture, drawn only while something reads it. View it, or send it to a second output as a
monitor during the sound check. It is transparent around the meters, so a `tex.composite` lays
it over `/show` (or over a `tex.constant` for a solid backdrop).

Play music with a clear beat; the tempo is found after a few seconds.

Try:

- Another palette in `/colour`: `fire` or `magma` keep the dark ground, `neon` gives up the
  black for flat colour.
- `b` in `/punch`: the brightness between kicks (0.6); lower makes the kicks hit harder.
