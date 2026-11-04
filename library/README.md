# The content library

Ready-made effects, one component each. Most are content for scanned objects. They take what a
scan leaves behind (`scan.scene`'s and `scan.regions`' pictures, camera stills), never the face or
region numbers of one particular scene, so they fit any scanned scene. The last group is a stage
for music: worlds that a `chan.beat_grid` drives (a synthwave world, a valley at night), with
elements to swap in and out.

Each folder holds the component (`<name>.comp.ron`), its shaders and pictures, and `preview.png`.
For a scan effect the preview is the effect rendered headless on a real scan of a few crates with
a pumpkin on one of them, over a dim drawing of the scan's regions. For the music stage it is the
stage at 20 s into `examples/beat-grid/music.wav`, with `warmth` at 1.

## Using one

- **Add it:** the add dialog's `library` tab lists them marked `penumbra`; over the control channel
  `comp.load {"file": "<name>"}`. Adding copies the component's own files (shaders, textures)
  into the project's `library/<name>/`, where its nodes look for them, so the project keeps
  working without penumbra's folder and can change its copies. A file the project already has is
  kept as it is (`comp.load` lists what it `copied` and what it `kept`); delete the copy to get
  penumbra's again.
- **Wire it:** its inputs are its `tex.in` nodes, named after what they want (below). Connect
  with `node.connect {"from": "/regions_image", "to": "/water", "input": "regions"}`.
- **Set it:** the component shows its effect's params (`node.set {"path": "/water", "params":
  {"level": 0.2}}`); they are the shader's own, so `node.info` on the inner node and the shader
  file's `// @param` lines say what each does.
- **Time:** the moving effects have a `clock` param: seconds since the effect started, below 0
  the show time. Bind it to a `chan.clock` (`seconds`) so the effect starts from its beginning
  when its piece comes on.
- **Into the projectors:** every effect draws in the scan camera's picture. Each projector
  takes its share as [Scanning, 7. The network](../docs/scanning.md#7-the-network) describes:
  `tex.remap` through the projector's `proj_to_cam_smooth_N.exr`, times its blend weight
  `weight_N.png`. `tiles` can also draw straight in a projector's pixels (see there).

### The pictures they take

| Input | What | From |
|---|---|---|
| `labels` | faces and objects in one picture: red the face k / 255, green the object k / 255, as plain numbers | `scan_labels`' `labels` |
| `sdf` | signed distance from the objects' silhouette (red, in widths of the picture, negative inside) | `scan_labels`' `sdf` |
| `regions` | region labels, 8 bit | `tex.image_file` of `regions_camera.png` (`scan.regions`) |
| `objects` | object labels, 8 bit | `tex.image_file` of `objects_camera.png` (`scan.scene`) |
| `height` | height over the support | `tex.image_file` of `height_camera.exr` (`scan.scene`) |
| `uv` | a rectified face's u, v (red, green) and where it is (blue) | `tex.image_file` of `uv_K_camera.exr` (`scan.scene` with `rectify`) |
| `fine`, `wide` | a camera still on the objects, softened lightly and blurred wide | `photo_detail` |
| `photo` | a camera still of the scene lit white by the projectors | the scan's `photo_*.png` (or the sum of them) |

Effects that read region labels (`water`, `glow_rim`, `lantern`) take the region by its number
as a param: look it up by name in `regions_reply.json` (`scan.regions`), which also has the
straight corners `water` wants.

## The building blocks

### scan_labels

![scan_labels](scan_labels/preview.png)

The scan's faces and objects in one picture, ragged borders smoothed (each pixel takes the label
most common around it), and the objects' silhouette as a signed distance. Nearly every effect
below starts from these two.

- In: `faces` (`regions_camera.png` or `planes_camera.png`), `objects` (`objects_camera.png`),
  `uv` (optional, see `fill`).
- Out: `labels`, `sdf`.
- Params: `keep` the one object kept (0: all; the others count as nothing), `radius` of the vote,
  `fill` a rectified face whose outline is filled where the scan left holes (a glossy top),
  with its `uv` wired; `tolerance`, `min_area` of the silhouette's trace; `width`, `height` the
  camera's size (1920 x 1080).

### photo_detail

![photo_detail](photo_detail/preview.png)

A camera still kept only on the objects, lightly softened (`fine`) and blurred wide (`wide`).
Their difference is the surface's fine relief: a pumpkin's furrows are a few millimetres deep,
far below a scan's noise, but each is a dark line in a photo. The preview shows how much darker
each pixel is than around it, not the photo.

- In: `photo`, `labels`. Out: `fine`, `wide`. Params: `fine_size` (3), `wide_size` (24) in pixels.

## Faces and regions

### edges

![edges](edges/preview.png)

Thin glowing lines where two faces meet and round the objects.

- In: `labels`. Params: `line` width, `color`, `glow` (a soft halo), `objects` (lines round the
  objects too).

### pulse

![pulse](pulse/preview.png)

Every face lights up and fades, one after another, each in a colour of a small palette
(Halloween by default: orange, violet, poison green, amber, plum). The faces are traced into
shapes, `tex.shape_content` pulses them.

- In: `labels`, `gate` (optional: white where faces may light, e.g. only faces a projector
  reaches entirely). Params: `speed`, `stagger`, `scatter`, `depth` of the pulse; `count` and
  `c0` to `c4` the colours, or `palette` (a named palette such as `candy`, or hex colours: all
  of its colours, in place of the five); `gain`; `brightness`.

### sweep

![sweep](sweep/preview.png)

A scan line sweeping across everything the scan found, with a fading trail; the faces it passed
keep a faint tint of their own.

- In: `labels`. Params: `speed` (sweeps per second), `angle`, `line`, `trail`, `color`, `tint`,
  `pause`, `clock`.

### breathe

![breathe](breathe/preview.png)

Regions each in one colour of a four-colour palette, breathing slowly one after another from
left to right; now and then a wipe crosses the picture and lights the regions' outlines.

- In: `regions`; optional `table` (a 256 x 1 picture: per label at x = k, red the colour slot
  k / 255, green its phase 0..1; without it slot k * 3 mod count and the phase from left to
  right across the picture) and `outlines` (white lines; without them they are found from the
  regions, `line` wide).
- Params: `count`, `c0` to `c3`, `edge` (outline colour), or `palette` (a named palette such as
  `blood`, or hex colours: all of its colours in place of the four, the brightest for the
  outlines), `gain`, `base`, `period`, `spread`, `fade`; the wipe's `first`, `every`, `wipe`, `breadth`, `angle`, `turn`, `lines`, `lift`;
  `clock`.

## On a round object

### relight

![relight](relight/preview.png)

A lamp that is not there, going round the object. A scan's height cannot shade a body seen from
the front, so the body is taken as round (a dome on its silhouette) and the ribs come from the
photo.

- In: `labels`, `sdf`, `fine`, `wide`. Params: `radius` of the dome (about half the object's
  width), `ribs`, `speed` (turns per second), `elevation`, `ambient`, `color`, `rim`, `soften`
  (blur of the silhouette, so the dome has no crease), `clock`.

### furrows

![furrows](furrows/preview.png)

Glow from the furrows, as if a candle burned inside: where the photo is darker than around it,
it glows and flickers.

- In: `labels`, `sdf`, `fine`, `wide`. Params: `gain`, `threshold`, `relative` (1 for a photo, 0
  with the height as `fine`/`wide`), `flicker`, `color`, `core`, `crest`, `rim` (no glow this close
  to the silhouette).

### contours

![contours](contours/preview.png)

Contour lines of the object's height. Bind `offset` (0 to `interval`) to a clock to make them
climb.

- In: `height`, `labels`. Params: `interval`, `offset`, `thickness`, `smooth`, `color`, `mode`,
  `range`.

### glow

![glow](glow/preview.png)

The object glowing from inside like a lantern: the skin brightest round a flame low in the
middle, the furrows hotter and yellower, all flickering as one flame that sputters up when it
starts. No light on the room (a projector's real shadows would give that away).

- In: `labels`, `sdf`, `fine`, `wide`. Params: `box` (the object's bounding box in the camera's
  pixels: left, top, right, bottom), `flame` (where in the box), `spread`, `skin`, `furrow`,
  `threshold`, `full`, `edge`, `flicker`, `sway`, `ignite`, `dim`, `hot`, `clock`.

### glow_rim

![glow_rim](glow_rim/preview.png)

A faint warm rim on the face the object stands on, right round its foot, flickering with `glow`
(same flame).

- In: `regions`, `sdf`, `labels`. Params: `lid` (the region it stands on), `box` (as `glow`'s),
  `reach`, `below`, `gain`, `flicker`, `ignite`, `color`, `clock`.

### candle

![candle](candle/preview.png)

The object as the scene's light: warm, flickering light on the faces round it, falling off with
the distance from its silhouette.

- In: `labels`, `sdf`. Params: `radius`, `intensity`, `flicker`, `color`, `wobble`.

### lantern

![lantern](lantern/preview.png)

A carved jack-o'-lantern face on a round object, curving with its surface: it ignites, looks
around, blinks, turns from calm to sly to scary, laughs and blows out (one loop of `length`
seconds).

- In: `labels`, `regions` (for the spill round its foot), `photo` (optional: the still of the
  projector the face comes from, to even out its light).
- Params: the sphere it is drawn on, `center`, `size`, `stem` (camera pixels; take them from the
  object's bounding box); `object`, `lid`; `part` (0 skin and spill, 1 the face alone, 2 both: a
  blend seam through a face looks bad, so give the face to the one projector that sees it best,
  unweighted, and the skin to all); `mouth` (bind to an audio level); `yaw`, `drop`, `scale`,
  the eyes; the light's colours; `seek`, `length`, `clock`.

## On faces of a box

### water

![water](water/preview.png)

A box seen at a corner (front, side, top) as a tank of water: it fills once, then stays, waves
rolling along the front and round the corner, caustics below; a pond on the top with rings from
the object standing on it, whose lower half catches the water's light.

- In: `regions`, `labels`, `sdf`. Params: `front`, `side`, `lid` (regions), `front_top`,
  `front_bottom`, `side_top`, `side_bottom` (their straight corners from `regions_reply.json`,
  top left and top right, then bottom left and bottom right), `rows` (the object's top and
  bottom row), `size` (the box in centimetres), `fill`, `pond`, `level`, `wave`, `tile`,
  `caustic`, `gain`, the colours, `shimmer`, `texture` (a tiling water picture), `clock`.

### tiles

![tiles](tiles/preview.png)

Glazed tiles with grout and a wooden rim on a rectified face, appearing one by one with
`reveal`.

- In: `uv` (`uv_K_camera.exr`), `objects` (`objects_camera.png`). For a projector's own pixels,
  load a second copy with `uv_K_N.exr` and `labels_N.png`: exact even where the camera measured
  nothing (a glossy top).
- Params: `object` (left out, e.g. what stands on the face), `reveal`, `aspect` (the face in
  centimetres, as given to `scan.scene`), `tile`, `grout`, `rim`, `decor` (share of decorated
  tiles), `grout_color`, `wood`, `gain`, `seed`, `atlas` (the decorated tile in the left half, a
  plain one in the right).

## A stage for music

A world that moves with a sound file: a stage (a synthwave floor, a tunnel, a tower to climb or
a lake in a valley at night), and elements in it.
They all take the same music bus, the whole output of a `chan.beat_grid`. A project has one
`chan.beat_grid` and wires it into the `bus` input of the stage and of every element:
`node.connect {"from": "/grid", "to": "/stage", "input": "bus"}`. Each component reads the
channels it needs by name. `travelled` moves the world, `drift` puts the lane's turns on
downbeats, `beat` and `beat_length` time the blocks, `energy_low` brightens the sky. [Beats of a
sound file](../docs/audio.md#beats-of-a-sound-file) explains these channels.

- **Floor elements** stand on the stage's grid and move with it. They take `bus` and the stage's
  `lane` output, so they keep the glider's lane free. Their output goes into the stage's `floor`;
  several go through a `geo.merge`.
- **Mid elements** fly in front of the camera, on the lane. They take `bus` and `lane` too. Their
  output goes into the stage's `mid`; several go through a `geo.merge`.
- **Ceiling elements** hang from the ceiling grid, 4 m above the floor. They take `bus` and
  `lane` and give geometry, as floor elements do. Their output goes into the stage's `ceiling`;
  several go through a `geo.merge`. Towards `presence` 0 they draw up into the ceiling. Over the
  lane they stay above 3.2 m, so the glider and the camera pass under them at every preset.
  Anything that hangs lower stays beside the lane.
- **Sky elements** draw a picture over black. Their output goes into the stage's `sky`, screened
  over the nebula. Several go through a `tex.composite` with `op: screen`. The sky lies behind
  everything the stage renders. The solid bodies of floor and mid elements are a plain
  `mat.occluder`: they hide the lines behind them and show the sky, so a body reads as a dark
  shape of nebula with its own edges. Screening only adds light, so a sky element cannot darken
  the nebula. Every sky element
  except `sky_galaxy` also takes the stage's `pose`, so its horizon moves with the camera.
- **Outside elements** are geometry far outside `stage_ascent`'s tower, such as ships. Their
  output goes into the ascent's `outside` input; several go through a `geo.merge`. The ascent
  draws them behind the tower, without its distance fade, so they stay sharp hundreds of metres
  away. Their solid bodies are black and hide the nebula behind them. Only `stage_ascent` has
  this input.
- **Post elements** change the finished picture. Each takes a picture in `in` and `bus`, and
  gives the changed picture in `out`. Chain them after the stage's `out`: stage, then
  `post_scanlines`, then the next one, then the output. At `presence` 0 a post element passes the
  picture on unchanged.
- **`presence`** (0 to 1) brings an element in and takes it out again. A floor element sinks into
  the floor towards 0, a mid element flies away, a sky element dims. To swap two elements, ramp
  one's `presence` down while the other's goes up, for example through the blend between two
  sections.
- **Elements that are out cost nothing** once the slot's `geo.merge` or `tex.composite` knows
  their `presence`. Wire a `chan.select` of the presence channels into its `show` input, in the
  order of its inputs: `blocks mountains pillars` for a merge of `/blocks` in `in0`, `/mountains`
  in `in1` and `/pillars` in `in2`. An input whose channel is 0 then does not cook, nor does
  anything inside its element. A stage of many elements runs about three times faster that way.
  Leave out an element that fades out by itself after its `presence` reaches 0: `mid_glider`
  flies away for a few seconds more. Put it after the inputs the channels name, so it always
  shows.
- **`beats.arrange`** does those swaps for a whole sound file. It writes a table with a row per
  section (the `presence` of each element, `warmth`, `intensity`) and binds the elements to it.
  Edit the table to change a section's picture. [An arrangement from the
  sections](../docs/audio.md#beats-of-a-sound-file) explains the rules. Name the slots `floor`,
  `ceiling`, `mid`, `sky` and `post`. `mid` and `sky` stay on through a break; `floor` and
  `ceiling` empty. In `post` every element is on or off by itself, so several can be on at once:
  they are off in an intro, outro and break and come on at a drop.
- **One element instead of another on a stage.** `mid_equalisers` is the tunnel's version of
  `sky_bars`: rings of bars to fly through instead of a row at the tube's far end. Give it to
  `beats.arrange` in the mid slot as `{path: "/equalisers", stages: ["/tunnel"], instead_of:
  "/bars"}`. In a tunnel row it then shows wherever `/bars` would, and `/bars` stays at 0. So
  the two never show at once in the tunnel. On the other stages `/bars` shows as before, and the
  rest of the table stays as it was without `/equalisers`.
- **Laser traps for the glider.** `mid_traps` fills stretches of the tunnel with laser beams,
  and the glider weaves through them. Give it to `beats.arrange` in the mid slot as `{path:
  "/traps", stages: ["/tunnel"], with: "/glider"}`, and wire its `corridor` output into the
  glider's `corridor` input. The traps then come with the glider: in a tunnel row of a build or
  a main section below energy 0.8, where the glider is the only mid element on. In those rows
  the `flight` column says 4, so the glider follows the free corridor through the beams. Every
  other cell stays as it was without the traps. See [mid_traps](#mid_traps).
- **Rings around the tower.** `mid_rings` puts broad rings around `stage_ascent`'s tower, outside
  its wall. Give it to `beats.arrange` in the mid slot as `{path: "/rings", stages: ["/ascent"],
  extra: "lively"}`. An extra takes no turns: the rings come by themselves in every second
  stretch of lively rows on the ascent (drops, builds, main sections from energy 0.8). Every
  other cell stays as it was without them. `extra: "calm"` brings an element in intros, outros
  and breaks instead. See [mid_rings](#mid_rings).
- **Ships beyond the tower.** `outside_ships` sends huge ships across the ascent's sky. Wire its
  `out` through a `geo.merge` (its `show` the ships' presence channel) into the ascent's
  `outside`. Bind the ascent's `outside` to the same channel, so the ascent skips that render
  while no ship shows. Give it to `beats.arrange` in the sky slot as `{path: "/ships", stages:
  ["/ascent"], extra: "calm"}`: the ships come in every second stretch of intros, outros and
  breaks on the ascent. See [outside_ships](#outside_ships).
- **A megastructure to climb through.** `outside_megastructure` puts a lattice of girders round
  the ascent's tower and a colossal ring round a glowing core far ahead. It has two outputs.
  Wire `near` into the mid slot's `geo.merge` and `out` into the outside merge, with the
  element's presence channel in both `show` inputs. Bind the ascent's `outside` to the larger
  of the ships' and the megastructure's presence (a `chan.math` with `op: max`). Give it to
  `beats.arrange` in the sky slot as `{path: "/megastructure", stages: ["/ascent"], extra:
  "steady"}`: it comes in every second stretch of main sections below energy 0.8 on the ascent.
  Those rows are neither lively nor calm, so it never shows with the rings or the ships. See
  [outside_megastructure](#outside_megastructure).
- **Lights for the room** come from `room_lights`: a picture for the room's columns and the
  channels for DMX lights, in the stage's colours and on its beat. Give it to `beats.arrange` as
  `lights: "/room"`, and its `pattern` changes with the sections. See [room_lights](#room_lights).
- **A room to judge it in** comes from `previs_room`: the stage on the main wall, the columns
  showing `room_lights`' picture and wash lights in its colours, seen from the audience. Record
  it with an `out.video` to judge the arrangement without standing in a room. See
  [previs_room](#previs_room).
- **The camera** takes one of six presets of the stage. The arrangement's `camera` column picks
  one per section, and the stage glides to it.
- **Several stages.** A project can hold several stages side by side: `stage_synthwave`,
  `stage_tunnel`, `stage_ascent` and `stage_valley` take the same inputs and give the same
  outputs. Each has its
  own floor, sky and ceiling elements. Wire their `out` into a `tex.switch` with `blend` on and
  that into the post chain. Give `beats.arrange` the stages as a list and the switch's param:
  `stage: ["/stage", "/tunnel", "/ascent"], switch: "/stages.index"`. The `stage` column then
  changes at a strong track start, at most once in 8 minutes. It walks to the next stage in the
  list and back again (0, 1, 2, 1, 0), so every change goes between neighbours. A blending
  switch fades between the two through the row's fade; with `blend` off it cuts half way
  through it. With `inactive: skip` on the switch only the stage on show renders, and both
  stages of a fade while it lasts. For two stages a `tex.cross` (`switch: "/stages.cross"`) fades
  too, but then both stages always render. The mid and sky elements can go into every stage. They
  read one `lane` and one `pose`. Blend the stages' lanes with a second blending `tex.switch` and
  give `switch` both params as a list: `switch: ["/stages.index", "/lanes.index"]`. Blend the
  poses with `chan.math`: take the `stage` channel of the arrangement's `_mix` (a `chan.select`),
  cut it into one share per fade (for three stages: its part up to 1 with `min` 1, and its part
  beyond 1 with `subtract` 1 and `max` 0), and add each neighbour's difference times its share
  to the first stage's pose. So the glider and the horizon move over with the picture.
  `intensity` takes a list too: `intensity: ["/stage.dim", "/tunnel.dim", "/ascent.dim"]`. Floor
  and ceiling elements need a flat floor and a flat ceiling: wire them into `stage_synthwave`
  only. An element that does not suit a stage gets a list of the stages it may show in:
  `{path: "/sun", stages: ["/stage"]}` keeps the sun off the tunnel and the shaft, and its slot
  shows another element there.
- **The stage's shape** is in its `pose` too: `axis` (metres up where the stage's middle lies:
  0.5 over the synthwave floor, the tube's axis, the shaft's middle), `walls` (1 where walls
  stand round the camera), `curve` (1 where the horizon is the round end of a tube), `radius`
  (the tube's or the shaft's radius, 0 on the plane) and `far` (metres ahead where the fade ends:
  the stage's `end`). The lasers start at `axis`, the glider floats up out of the picture where
  `walls` is 1. With `curve` the bars and the city stand round the tube's far end, `far` metres
  ahead: a ring of `radius` round the axis, which the camera sees above the vanishing point. The
  figures stand on that ring's bottom and the planet hangs inside it. Wire the blended pose into
  `mid_lasers`, `mid_glider`, `mid_equalisers`, `mid_traps`, `mid_rings` and the outside
  elements as into the sky elements.
- **Rolling the plane up into the tunnel.** `stage_synthwave` and `stage_tunnel` both have
  `roll_up`. Bind both to the share of the fade between them (the `min` 1 part of the `stage`
  channel above). While the switch fades, the plane's floor gives way to lines from right to
  left, and those lines bend up into the tunnel's rings; the ceiling grid fades out. Both
  stages show the same lines through the fade, so the switch's crossfade reads as one floor
  rolling up.
- **Turning the tunnel up into the ascent.** `stage_tunnel` and `stage_ascent` both have
  `turn_up`. Bind both to the share of the fade between them (the part of the `stage` channel
  beyond 1). While the switch fades, the tunnel's wall, long lines and chevrons go, and its
  rings thin out to free hoops, one every `storey_beats` beats. The hoops move apart to the
  storeys' spacing and widen to the tower's radius. Then the tube turns upright in front of the
  camera, and the hoops sink past it as storeys. The ascent draws the same hoops through the
  fade, lying down at first, and builds its posts and floors as it stands. Both stages show the
  same hoops at the same places, so the crossfade reads as one movement. The camera stays
  level: the hoops turn, not the camera. For the cameras to match, bind the tunnel's inner
  `toward` (a `chan.constant`: `height`, `pitch`, `roll`, `fov`, `side`) to the ascent's pose
  through a `chan.select`: the tunnel's camera then glides into the ascent's over the first
  third of the fade. The reverse walk runs the same way back.
- **Seeing through the tunnel.** `beats.arrange` binds `stage_tunnel`'s `see_through` to its
  `see_through` column. Now and then the tube's wall opens through a break or a build: every
  second stretch of breaks and builds on the stage. The wall stays closed in the rows on either
  side of a change of stage, so the roll-up and the turn-up look as before.

### stage_synthwave

![stage_synthwave](stage_synthwave/preview.png)

The world: a camera flying over a wireframe floor, under a wireframe ceiling, towards a nebula
in a warm or cold palette. The world moves one grid cell per beat of `travelled`. The lane
zigzags to the other side every two bars, each turn starting on a downbeat. The bass brightens
the nebula. Lines fade out with distance from the camera. The sky (nebula and `sky`) lies behind
the render: what the render draws solid hides it, and far things melt into it as they fade. The
glider is the element `mid_glider`.

- In: `bus`, `floor` (geometry standing on the grid), `mid` (geometry in front of the camera,
  such as the glider), `ceiling` (geometry hanging from the ceiling grid), `sky` (a picture
  screened over the nebula).
- Out: `out` (the picture), `lane` (256 x 1: per texel i, -8 + 0.25 i metres ahead, red the
  lane's x in metres, green its sideways slope per metre, blue `step`, alpha the floor: 1 where
  it stands, 0 in a gap), `pose` (the camera as it glides: channels `height`, `pitch`, `roll`,
  `fov`, `side`, and the stage's shape: `axis` 0.5, `walls` 0, `curve` 0, `radius` 0, `far` the
  fade's `end`; wire it into the
  sky elements and the mid elements that take it).
- Params: `warmth` (0 the warm palette, 1 the cold one), `lift` (added to the bass, e.g. before a
  change), `energy_curve`, `dim` (the nebula's light); the lane's `bars_per_turn`, `swing`, `turn`,
  `beats_per_bar`; `step` (metres per beat); `start`, `end` of the distance fade; `ground` (how
  dark the sky gets below the horizon: 0 not at all, the default, so the nebula shows under the
  floor's lines and in a gap; 1 black, so the floor stands on black), `roll_up` (0 the floor as it is; towards 1
  the floor grid gives way to lines from right to left that bend up into rings round an axis
  4 m up, as `stage_tunnel`'s rings at its own `roll_up`, and the ceiling grid fades out).
- Camera: `camera` (the preset, 0 to 5), `glide` (seconds the camera takes to reach a new preset:
  most of the way after about 3 times this), `presets` (a csv table, one row per preset: `height`
  in metres, `pitch` in degrees up, `roll` in degrees, `fov` in degrees, `side` in degrees to the
  right). The presets: 0 low flight at 1.5 m, 1 higher and looking down for a build, 2 high and
  wide for a drop (2.9 m, 74 degrees), 3 still and level for a break, 4 and 5 two tilted variants
  that the tracks take in turn. `pitch` and `side` stay good to about 30 degrees.
- Gaps: `gap` (the share of the floor grid that breaks off, 0 to 1; 0 keeps it whole),
  `gap_length` (metres a gap runs, rounded to whole bars). A gap begins where a bar begins, so its
  near edge passes the camera on a downbeat. The lane floats on over nothing. Floor elements read
  the gaps from `lane`: a thing whose cell of the floor is in a gap is left out whole, so no stub
  stands at a gap's edge. A change of `gap` reaches only the floor beyond the fade's `end`, out
  of sight: each stretch of the floor keeps the share it had when it was last out there. So a gap
  forms behind the horizon and comes towards the camera with the floor, and the floor comes back
  the same way. `gap_memory` (a `tex.shader` fed back through `gap_loop`, a `tex.feedback`)
  remembers the shares. `beats.arrange` binds `gap` to its `gaps` column: 0.3 in a build, 0.6 in
  a break.

### stage_tunnel

![stage_tunnel](stage_tunnel/preview.png)

A second world for the same elements: the camera flies through a wireframe tube towards the sky
at its end. The tube has a ring on every beat of `travelled` and long lines along its wall. It
rolls with the lane, so the lane always lies at the tube's bottom, straight ahead, and the long
lines twist as the lane zigzags. The wall is black and hides what lies outside the tube. Far
away the tube fades out, so the sky (nebula and `sky`) shows as a disc at its end. With
`see_through` the wall's black fades out, and the sky shows through the whole tube between its
rings and long lines. Palette,
nebula, distance fade and camera presets work as on `stage_synthwave`, so the two stages can
swap.

The tube lives with the music. On every beat a pulse of light runs from the camera down the
tube, and the rings it passes narrow a little. Twice a bar a bright dash runs ahead along every
third long line. Chevrons on the tube's bottom point ahead and come towards the camera with the
world; the bass brightens them. Every `roll_bars` bars the whole tube makes a turn round its
axis over `roll_length` bars, the next turn the other way; in a drop the turns come twice as
often. The tube is a sheet rolled up: at `roll_up` 0 it lies open as a flat floor of lines from
right to left, the rings, which roll up into the tube as `roll_up` grows (see the paragraph on
rolling the plane up above).

- In: `bus`, `mid` (geometry in the tube, such as the glider), `sky` (a picture screened over
  the nebula), `floor` and `ceiling` (merged into the scene, but the tube has no flat floor or
  ceiling: floor elements beside the lane stand through the curved wall, and ceiling elements
  hang at their own `ceiling` height in the middle of the tube; leave both unwired).
- Out: `out` (the picture), `lane` (the layout of `stage_synthwave`'s lane: red and green 0,
  since the lane lies straight at the tube's bottom, blue `step`, alpha 0, since nothing can
  stand on the curved bottom), `pose` (the camera as it glides: channels `height`, `pitch`,
  `roll`, `fov`, `side`, and the stage's shape: `axis` the tube's axis, 4 m, `walls` 1, `curve`
  1, `radius` 4 m, `far` the fade's `end`, 20 m).
- Params: `warmth` (0 the warm palette, 1 the cold one), `lift`, `energy_curve`, `dim`; the
  lane's `bars_per_turn`, `swing`, `turn`, `beats_per_bar`; `step` (metres per beat: the
  distance between two rings); `start`, `end` of the distance fade (6 and 20 m: the tube is
  seen further than the synthwave floor); `ceiling` (the tube's height, twice its radius: 8 m);
  `twist` (how far the tube rolls with the lane: 1 turns it by the lane's swing over the
  radius); `lines` (long lines along the wall, up to 24); `line_width`; `roll_up` (1 the tube,
  towards 0 a flat floor of lines); `roll_bars` (bars from one turn of the tube to the next, 8;
  0 never turns); `roll_length` (bars a turn takes, 2); `pulse` (the light of the pulses and
  dashes, 1; 0 none); `marks` (the chevrons' light, 1; 0 none); `turn_up` (0 the tube, towards 1
  hoops that turn upright into a tower; see the paragraph on turning up above); `storey`,
  `storey_beats` and `tower_radius` (where the hoops end up: 3 m apart, one every 4 beats, 6 m
  wide, as on `stage_ascent`). `curve` in the pose falls to 0 as the tube turns up.
  `see_through` (0 the black wall, the default; towards 1 the wall's black fades out, so nebula
  and `sky` show through the whole tube; rings, long lines and chevrons stay). A change of
  `see_through` takes about a second, so it never switches. The wall still hides what lies
  outside the tube, such as debris that flies beyond it. `beats.arrange` binds it to its
  `see_through` column.
- Camera: `camera`, `glide` and `presets` as on `stage_synthwave`, with presets of the same
  meaning for the tube: 0 low flight at 1.8 m, 1 higher and looking down for a build, 2 high and
  wide for a drop (3.4 m, 76 degrees), 3 still and nearly level for a break, 4 and 5 two tilted
  variants.
- Gaps: `gap` and `gap_length` as on `stage_synthwave`: stretches of the tube run out, and the
  sky shows through them. A gap starts on a bar, and a change of `gap` comes in from the far end,
  as on `stage_synthwave`. Over its first and last metres (at most 4 m) the
  rings thin out: every second ring stays, then every fourth, on the downbeats. The long lines
  and chevrons fade, and the wall opens from the top down in narrowing slats. In the middle of
  the gap nothing is left, and the tube comes back the same way.

### stage_ascent

![stage_ascent](stage_ascent/preview.png)

A third world for the same elements: the camera climbs inside a wireframe tower. Each storey is
a black floor along the tower's wall with glowing edges, open in the middle where the camera
rises. A storey passes downwards every `storey_beats` beats of `travelled`. Posts and braces of
lines join the storeys. The tower turns slowly around its axis, once in `orbit` beats, and sways
with the lane, so the camera seems to circle inside the shaft. The walls are open: the sky
(nebula and `sky`) shows through them, and the storeys high above fade into it. The mid elements
fly ahead of the camera as on the other stages, straight through the middle of the shaft.
Palette, nebula, distance fade and camera presets work as on `stage_synthwave`, so the stages can
swap.

- In: `bus`, `mid` (geometry in the shaft, such as the glider), `sky` (a picture screened over the
  nebula), `outside` (geometry far outside the tower, such as `outside_ships`: drawn behind the
  tower through the same camera, out to 5 km, without the distance fade), `floor` and `ceiling` (merged into the scene, but the shaft has no floor: floor
  elements sink away, since the lane's alpha is 0, and ceiling elements hang at their own height
  in the middle of the shaft; leave both unwired).
- Out: `out` (the picture), `lane` (the layout of `stage_synthwave`'s lane: red and green 0,
  since the lane runs straight ahead, blue `step`, alpha 0, since there is no floor to stand
  on), `pose` (the camera as it glides: channels `height`, `pitch`, `roll`, `fov`, `side`;
  `height` is the camera's height above the floor it would stand on, the same on every storey;
  and the stage's shape: `axis` 1.5, `walls` 1, `curve` 0, `radius` the shaft's, `far` the
  fade's `end`).
- Params: `warmth` (0 the warm palette, 1 the cold one), `lift`, `energy_curve`, `dim`; the
  lane's `bars_per_turn`, `swing`, `turn`, `beats_per_bar` (they set the sway); `step` (metres per
  beat, for the mid elements); `start`, `end` of the distance fade (8 and 24 m); `ceiling` (metres
  from one storey to the next: 3); `storey_beats` (beats to climb one storey: 4); `orbit` (beats
  for one turn of the tower: 128); `radius` (the tower's radius: 6 m; the camera rises halfway
  between its axis and its wall); `twist` (how far the tower sways with the lane: 1 turns it by
  the lane's swing over the radius); `lines` (posts around the tower, up to 24); `line_width`;
  `turn_up` (1 the tower stands; towards 0 it lies down along the view as a tube of hoops, the
  way `stage_tunnel` turns up); `tube_radius` (the lying tube's radius: 4 m, as the tunnel's);
  `outside` (1 draws the `outside` input, 0 skips its render: bind it to the presence of what
  goes in, so an empty input costs nothing).
  `curve` in the pose rises to 1 as the tower lies down.
- Camera: `camera`, `glide` and `presets` as on `stage_synthwave`, with presets of the same
  meaning for the shaft, all looking up a little: 0 at 1.5 m and 8 degrees up, 1 lower and looking
  up more for a build, 2 wide for a drop (80 degrees), 3 still and nearly level for a break, 4 and
  5 two tilted variants. The heights stay near the glider's, so the glider stays in view.
- Gaps: `gap` (the share of the storeys that break off, 0 to 1) and `gap_length` (metres a gap
  runs up the tower, rounded to whole storeys). In a gap only the posts and braces stand, and
  the sky shows between them.

### stage_valley

![stage_valley](stage_valley/preview.png)

A second family of worlds: the camera flies low over a dark lake in a valley at night, towards
the first light of dawn on the horizon. The sky is night blue near the horizon and black
overhead. The dawn lies on the horizon as a band of light: `dawn` 0 is a thin dark red line, 1 a
broad copper band with a gold core. The band never becomes a sun. The lake mirrors the sky, the
band and the stars, blurred and broken up by ripples. The mirror is faint near the camera and
strong towards the horizon. A low mist drifts on the water near the camera; the bass thickens
it. Ripples and mist come towards the camera one grid cell per beat of `travelled`, as the
synthwave floor does. The sky elements are `sky_stars`, `sky_horizon_glow`, `sky_volcano` and
`sky_clouds`. On the shore stand the floor elements `floor_terraces`, `floor_reeds` and
`floor_stelae`.

The stage has a slow signal of its own, the swell. It rises over about 8 seconds while the music
is quiet (a breakdown) and over the 8 bars before a new section (the grid's `build`), and it
falls over about 3 seconds when the kick comes back. The swell lifts the band and makes it
glow. So a breakdown raises the light on the horizon, and the drop takes it back.

- In: `bus`, `floor`, `mid`, `ceiling`, `sky` (a picture screened over the night sky), as on
  `stage_synthwave`. The stage has no floor grid: floor elements stand on the shore, and the
  lane's alpha is 1 where the shore stands.
- Out: `out` (the picture), `lane` (the layout of `stage_synthwave`'s lane), `pose` (the camera
  as it glides, and the stage's shape: `axis` 0.5, `walls` 0, `curve` 0, `radius` 0, `far` the
  fade's `end`, 40 m; plus `swell` and `dawn`, 0 to 1, for elements that follow them).
- Params: `warmth` (0 the ember palette with an amber glow above the band, 1 the jade palette
  with a cold blue-green glow and a greener mist), `lift`, `energy_curve`, `dim` (the sky's
  light: 1 full, 0 half; `beats.arrange` binds it as `intensity`), `dawn` (0 to 1: bind it to
  the file's `progress`), `swell_rise` (8 s) and `swell_fall` (3 s), `band_height` (how far the
  band reaches up at `dawn` 1, in picture heights: 0.1), `band_width` (how far it reaches to
  either side at `dawn` 1: 0.9). The lane's `bars_per_turn` (4: one turn every 4 bars, slower
  than on `stage_synthwave`), `swing`, `turn`, `beats_per_bar`; `step`; `start`, `end` of the
  distance fade (12 and 40 m: the valley is wide). The water: `ground` (1: below the horizon
  only the water shows; 0 the sky elements show through it), `reflection` (1), `ripple` (1),
  `mist` (1; 0 none), `shore` (how much of the reflection shows where the shore stands: 0.5).
- Camera: `camera`, `glide` (3 s) and `presets` as on `stage_synthwave`, lower and slower: 0 low
  flight at 1.2 m, 1 at 1.8 m looking down a little for a build, 2 high and wide for a drop
  (2.4 m, 70 degrees), 3 level for a break at 1.6 m, 4 and 5 two tilted variants (0.9 m and
  1.8 m).
- Gaps: `gap` and `gap_length` as on `stage_synthwave`. Stretches of the shore fall away into
  open water. Floor elements leave those stretches out. The open water mirrors the sky more
  clearly and has less mist. A gap comes in from beyond the horizon, as on `stage_synthwave`.

### floor_blocks

![floor_blocks](floor_blocks/preview.png)

A city of wireframe blocks. Each block lies flat until it passes `rise_at` and shoots up on the
next beat, so a new row stands up on every beat. Blocks on the lane stay low.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the blocks sink back into the floor one after another), `rise`
  (beats a block takes to stand up), `rise_at`, `lane` (half the low lane's width), `fill` (share
  of places with a block), `spread` (the city's width), `near`, `far`, `extent`.

### floor_mountains

![floor_mountains](floor_mountains/preview.png)

Wireframe mountain ranges beside the lane: square pyramids with contour lines, overlapping into
jagged ridges. They rise from the lane's edge to full height over `foot` metres; no foot reaches
into the lane. The heights come from noise fixed to the floor, so a ridge keeps its shape as it
comes nearer. Another `seed` gives another range.

- In: `bus`, `lane`. Out: `out`.
Peaks lower than 0.9 m, or lower than half their width, are left out. Near that height a peak
shrinks whole instead of flattening, so no flat plates appear. Just below it a peak shrinks
away over 20 cm instead of vanishing at once, so a peak right at the limit does not blink. On
each beat the peaks nod: they grow taller by `nod` and settle back over the beat.

- Params: `presence` (towards 0 the peaks shrink away one after another), `seed`,
  `height` (the tallest peaks, metres; the camera is 2 m up, the ceiling 4 m), `width` (a peak's
  foot across), `length` (along the lane, as a multiple of `width`), `spacing` (between peaks),
  `peaks` (metres between summits: smaller is more jagged), `valleys` (peaks lower than this share
  of `height` are left out, so the floor shows between ridges), `foot`, `lane` (as on
  `floor_blocks`, 1.6), `spread` (the range's width, both sides), `near`, `far`, `nod` (how much
  taller the peaks grow on each beat, as a share of their height, 0.2; 0 keeps them still),
  `color`, `extent`.

### floor_pillars

![floor_pillars](floor_pillars/preview.png)

Tall wireframe pillars in lines on both sides of the lane, like an avenue. A pillar stands every
`spacing` bars of travel, so the pillars pass the camera on downbeats. The second line on a side
stands half a gap later, so the two lines interleave. The lines follow the lane's zigzag at a
fixed distance from its edge. On each beat the pillars stretch a little. No pillar is lower than
1.2 m: between so many upright lines a stump looks wrong.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the pillars shrink away whole, one after another; at a steady
  presence a pillar stands at full size or not at all), `spacing` (bars between two pillars),
  `height` (metres, at least 1.2; the camera is 2 m up, the ceiling 4 m), `width`, `rows` (lines
  on each side, 1 to 3, default 2), `gap` (metres between lines), `pulse` (how far they stretch on
  a beat), `lane` (as on `floor_blocks`, 1.6), `near`, `far`, `beats_per_bar`, `color`, `extent`.

### floor_hexfield

![floor_hexfield](floor_hexfield/preview.png)

A dense field of hexagonal wireframe prisms side by side on both sides of the lane, like basalt
columns. The prisms stand in a honeycomb fixed to the floor, so the field comes nearer with the
floor. Their heights come from noise fixed to the floor, from 0.4 to 3.2 m, so hills and valleys
of prisms keep their shape as they come nearer. On each beat the prisms rise a little. The lane
stays free: prisms at its edge shrink whole. The bodies hide the lines behind them.
`hexfield.py` in the folder writes the model.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the prisms sink into the floor one after another), `seed`,
  `radius` (metres from a prism's middle to a corner), `fill` (how much of its cell a prism
  fills; 1 without a gap), `lowest`, `tallest` (metres), `hills` (metres between two hills: smaller
  is more broken), `pulse` (how far they rise on a beat), `lane` (1.6), `near`, `far`, `color`,
  `line_width`, `extent`.

### floor_hexgap

![floor_hexgap](floor_hexgap/preview.png)

Split hexagonal pillars beside the lane. Each pillar is a base standing on the floor and a
counterpart hanging from the ceiling, with open air between them. The pillars stand where
`floor_pillars` puts its pillars: in lines on both sides of the lane, one every `spacing` bars
of travel. Every pillar has a gap of its own: its middle lies between 1.2 and 3 m up, and it is
0.3 to 1 m tall. On each beat the gaps open a little. The bodies hide the lines behind them.
`hexgap.py` in the folder writes the model.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the bases sink into the floor and the counterparts into the
  ceiling, one pillar after another; at a steady presence a pillar stands whole or not at all),
  `ceiling` (the ceiling's height the counterparts hang from, 4 m), `seed` (other gaps), `low`,
  `high` (the lowest and highest middle of a gap, 1.2 and 3 m), `narrow`, `wide` (the smallest
  and largest gap, 0.3 and 1 m), `pulse` (how far a gap opens on a beat, as a share of its
  height), `spacing`, `width`, `rows`, `gap` (metres between lines), `lane` (1.6), `near`, `far`,
  `beats_per_bar`, `color`, `line_width`, `extent`. A pillar over a gap in the floor is left out
  whole, both parts.

### floor_waves

![floor_waves](floor_waves/preview.png)

Waves beside the lane: 48 lines across the floor, each a height field of ridged noise. The lines
move with the floor, and the waves roll towards the camera a little faster still. Each wave hides
the lines and floor behind it, like a range of hills. The bass raises the waves. The lane stays
flat, so the glider never flies through a wave.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the lines flatten into the floor one after another), `source`
  (`noise`: the waves rise with the bass; `levels`: bass beside the lane, mids further out,
  treble at the edges), `seed`, `height` (the highest crests, metres), `bass` (share of the height
  that follows the bass), `crests` (metres between crests), `flow` (metres per beat the waves roll
  on), `spacing` (metres between lines), `spread` (the field's width), `lane` (half the flat
  lane's width, 1.6), `foot` (metres beside the lane over which the waves rise), `near`, `far`,
  `smooth` (seconds the levels take to fall), `color`, `line_width`, `extent`.

### floor_cracks

![floor_cracks](floor_cracks/preview.png)

Glowing cracks in the floor, like lava under a crust: the edges of a cell pattern fixed to the
floor. Waves of light run along the cracks towards the camera. The cracks glow brighter with the
bass, and their cores turn yellow-white on a strong bass. The lane stays dark. The cracks lie on a
sheet 1.2 cm above the grid. Where the sheet is dark it is transparent, so the grid shows through.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the cells go dark one after another), `seed`, `bass` (share of
  the glow that follows the bass), `cell` (metres across a cell), `thickness` (metres, the bright
  core), `glow` (metres, the light around it), `flow` (metres per beat the light runs), `pulse`
  (metres between two waves of light), `color`, `hot` (the core at full bass), `brightness`, `lane`
  (half the dark lane's width), `fade` and `gone` (metres ahead where the cracks start to fade and
  where they are gone), `smooth` (seconds the bass takes to fall).

### floor_palms

![floor_palms](floor_palms/preview.png)

Wireframe palms in a line on each side of the lane, leaning away from it. A palm stands every
`spacing` bars of travel: the left ones pass the camera on a downbeat, the right ones half way
between. Each palm has a trunk of rings and 7 fronds. The fronds swing up and down, one swing
per two beats, wider with the mids. `palm.py` in the folder writes the two models.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the palms shrink into the floor one after another), `spacing`
  (bars between two palms of a line), `size` (1 is 2.6 m to the crown), `fronds` (3 to 8),
  `sway` (degrees the fronds swing at full mids), `calm` (share of the swing left in quiet mids),
  `droop` (degrees the fronds hang), `lane` (1.6), `clear` (metres between the lane's edge and a
  trunk, room for the fronds), `near`, `far`, `beats_per_bar`, `smooth` (seconds the mids take to
  fall), `color`, `line_width`, `extent`.

### floor_animal

![floor_animal](floor_animal/preview.png)

A wireframe horse galloping beside the lane. It runs a little faster than the flight for a while,
then a little slower, so it gains on the camera and falls back again, 3.5 to 6.5 m ahead. One
cycle takes 16 to 32 bars; `seed` sets its length. The legs follow the horse's way over the
floor: one stride per 1.155 m along its body, so a hoof on the ground stands still on the floor.
At the stage's 0.5 m per beat that is a stride every 2 beats, a little faster while the horse
gains on the camera. The stride is a slow gallop in the order Muybridge photographed: the hind
legs land, then the forelegs, then all four leave the ground. The body rocks, the neck swings, the
tail streams back. The horse faces 30 degrees in towards the lane, so the camera sees its flank.
Its body hides the lines behind it. `animal.py` in the folder writes the 24 poses.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the horse runs off ahead and shrinks, 0 none), `at` (metres
  ahead on average), `range` (metres it gains and falls back), `cycle_min` and `cycle_max` (bars
  of one cycle), `seed`, `side` (1 right of
  the lane, -1 left), `lane` (1.6), `clear` (metres from the lane's edge to the horse's middle),
  `turn` (degrees it faces in towards the lane), `size` (1 is 1.6 m to the withers),
  `beats_per_bar`, `color`, `line_width`.

### floor_title

![floor_title](floor_title/preview.png)

The track's title in huge letters lying across the lane, for the camera to fly over. At the
start of a track the first copy lies 4 m ahead, and one letter appears with each beat. The copy
keeps its distance until the last letter is there. Then it stays behind on the floor, and the
camera flies over it. Another copy follows every 8 bars of travel, all its letters there. The letters
follow the lane's bends, run through the palette from the first to the last, and sink into gaps
in the floor. `text` is the title. With `source: track` the letters say `TRACK` and the track's
number instead; the grid's sections know a track's number but no name. The bus channel
`track_bars` times the letters.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (from 0.4 every letter, below it the letters go out one after another),
  `text`, `source` (`text` or `track`), `warmth`, `ahead` (metres to the first copy while
  its letters appear), `every` (bars between copies), `long` (metres a letter reaches along the
  floor), `wide` (metres from one letter to the next), `span` (metres the whole title takes at
  most), `brightness`, `rim` (how bright the outline is), `beats_per_bar`, `fade` and `gone`
  (metres where the letters fade out). The font is monospaced, so each letter has a place of its
  own: `JetBrains Mono` inside.

### floor_terraces

![floor_terraces](floor_terraces/preview.png)

Stepped platforms on the shore of `stage_valley`, black against the night, with a thin rim of
copper light on their edges. They stand in two lines on each side of the lane and pass with the
floor. Each platform has three tiers that step up away from the lane. The near line is lower
than the camera, the back line taller. About one place in five on the back line holds a small
stepped pyramid instead, its stair turned towards the lane and the camera. Some places stay
empty, so water shows between the platforms. The bass brightens the rims, and each beat adds a
small pulse; the shapes stay still. The rims brighten a little towards `dawn` 1. `terraces.py`
in the folder writes the models.

- In: `bus`, `lane`, `pose` (the stage's: `dawn` comes from it). Out: `out`.
- Params: `presence` (towards 0 the platforms sink into the water one after another; at a
  steady presence a platform stands whole or not at all), `spacing` (bars between two places,
  3), `fill` (share of the places that hold something, 0.75), `height` (the near line, metres,
  0.9), `back` (the back line's height as a multiple of `height`, 2.4), `depth` (metres a
  platform reaches back, 2.4), `pyramids` (share of the back line's places with a pyramid,
  0.2), `pyramid` (a pyramid's size: 0.75 is about 4 m wide and high), `face` (degrees a stair
  turns towards the camera, 25), `rim` (brightness of the rims), `pulse` (how much the rims
  brighten on a beat), `warmth` (0 copper rims, 1 a cooler pale bronze; `beats.arrange` binds
  it), `lane` (1.6), `clear` (metres between the lane's edge and the near line, 2.2), `near`,
  `far` (36 m), `beats_per_bar`, `smooth` (seconds the bass takes to fall), `line_width`,
  `extent`.
- The platforms sink below a sheet at the water line that hides what lies under it. Floor
  elements in the same render cannot reach below the water line while this element is on.

### floor_reeds

![floor_reeds](floor_reeds/preview.png)

Reeds at the water's edge of `stage_valley`: two lines of clumps on each side of the lane, thin
dark lines with jade tips. A clump stands about every 0.55 m, and a few places stay empty. The
reeds sway across the lane in a wave that runs along the line towards the camera. The bass
widens the sway. The stage's swell lights the tips: faint in a loud section, a jade glint in a
breakdown, each clump catching the light at its own moment. All reeds are one
`geo.tex_instance` of one clump, so they cost little. `reeds.py` in the folder writes the clump.

- In: `bus`, `lane`, `pose` (the stage's: `swell` comes from it). Out: `out`.
- Params: `presence` (towards 0 the clumps shrink into the water one after another), `spacing`
  (metres between two clumps of a line), `fill` (share of the places with a clump, 0.8), `size`
  (a clump's height, metres, 1.2), `sway` (degrees the reeds lean to either side at full bass,
  7), `calm` (share of the sway left in a quiet bass, 0.3), `wave` (metres between two crests of
  the sway), `glint` (how bright the tips glint at full swell, 3), `lane` (1.6), `edge` (metres
  between the lane's edge and the first line, 0.2), `depth` (metres between the two lines),
  `near`, `far` (30 m), `smooth` (seconds the bass takes to fall), `line_width`, `colors` (the
  ramp from the foot of a blade, position 0, to its tip, 1).

### floor_stelae

![floor_stelae](floor_stelae/preview.png)

Upright stones on the turns of `stage_valley`'s lane, each with a carved glyph on its front. A
stela stands on the outside of every turn, close to the lane, its front turned towards the lane
and the camera. The glyph is one of three: a squared spiral, a step fret (a staircase ending in a
hook) or a square of dots, each over a number in dots and bars. The light comes from the band on
the horizon: the grooves stay dark, their lower walls and the rims catch copper light. The light
grows towards `dawn` 1 and with the stage's swell, and each beat adds a small pulse. The stelae
find the turns in the stage's `lane` picture, so they follow any `bars_per_turn`. `stelae.py`
in the folder writes the models.

- In: `bus`, `lane`, `pose` (the stage's: `dawn` and `swell` come from it). Out: `out`.
- Params: `presence` (towards 0 the stelae sink into the water one after another; at a steady
  presence a stela stands whole or not at all), `seed` (other glyphs and sizes), `height`
  (metres, 2.4), `width` (metres, 0.9), `thick` (metres, 0.35), `light` (brightness of rim and
  glyph), `pulse` (how much the light brightens on a beat), `face` (degrees the front turns
  from the lane towards the camera, 30), `warmth` (0 copper light, 1 jade; `beats.arrange`
  binds it), `lane` (1.6), `clear` (metres between the lane's edge and a stela, 1.3), `near`,
  `far` (36 m), `smooth`, `groove` (how bright a groove's floor is), `line_width`.
- The stelae sink below a sheet at the water line, as `floor_terraces` do.

### mid_glider

![mid_glider](mid_glider/preview.png)

The glider: a wireframe hover craft flying ahead of the camera along the lane. It turns with the
lane, leans into each turn and bobs slowly. Towards `presence` 0 it climbs away ahead, up to 6 m
further and 1 m higher, and shrinks over the last quarter; at 0 it is gone. On a stage with walls
(`walls` 1 in the stage's `pose`) it would fly through the wall that way: there it floats
straight up out of the picture instead, and comes back down from above. The glider smooths
`presence` itself, so it glides in and out over about 4 seconds even when the arrangement fades
faster. In a break `beats.arrange` keeps it at 0.6: about 2 m further ahead and 0.35 m higher.

The glider flies in one of four ways, picked by `flight`:

- 0 slalom: along the lane's turns, leaning into each one.
- 1 straight: down the lane's middle, nose ahead and level. On the synthwave stage the middle
  stays inside the free lane, so floor elements stay clear of it.
- 2 roll: the slalom, and once every `roll_bars` bars the glider rolls round its length on the
  downbeat, the other way round each time.
- 3 lean: straight, and on each downbeat the glider leans to one side and drifts a little that
  way, back over the bar, the other side on the next bar.
- 4 thread: along the free corridor through the beams of `mid_traps`, 2 m further ahead, nose
  half along the corridor and leaning into its turns. The glider reads the corridor from its
  `corridor` input; wire the traps' `corridor` output there. Unwired, it follows the traps'
  default corridor.

A change of `flight` glides over about 2 seconds. `beats.arrange` picks one per section in its
`flight` column: straight in a break, the roll on a drop, the lean in a build, and in the other
sections slalom, straight and lean by turns from track to track. Where `mid_traps` comes with
the glider, the column says thread.

- In: `bus` (`travelled` and `drift` time the roll and the lean), `lane`, `pose` (the stage's,
  for `walls`; unwired, it climbs away ahead), `corridor` (the `corridor` output of
  `mid_traps`). Out: `out` (into the stage's `mid`).
- Params: `presence`, `at` (metres ahead of the camera), `flight` (0 to 3, above), `arrive`
  (seconds to cover 63 % of a change of `presence`, default 2), `bank` (how far it leans into a
  turn), `bob` (metres up and down), `scale` (0.16 makes it 2.4 m long), `hover` (metres above
  the floor), `flights` (a csv table, one row per flight: `follow` 1 along the turns or 0
  straight, `roll` 1 to roll, `lean` 1 to lean on the downbeat, `thread` 1 along the traps'
  corridor; shares between mix the two),
  `flight_glide` (seconds to cover 63 % of a change of flight, default 0.8), `roll_bars` (bars
  from one roll to the next, default 4), `roll_beats` (beats a roll takes, default 2),
  `lean_angle` (degrees of the lean, default 22), `beats_per_bar`.

### mid_particles

![mid_particles](mid_particles/preview.png)

A warp stream: short white streaks fly at the camera, much faster than the floor, and pass it on
all sides. Busy music sends more of them; faster music draws them longer. They keep out of the
camera's line of sight and out of the glider's space, low in the middle.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence` (towards 0 the stream thins out), `density` (the share of streaks in quiet
  music; onsets raise it to all), `busy` (seconds the onsets take to settle), `speed` (times the
  floor's speed), `length` (metres at 120 BPM), `spread` (half the stream's width), `near`, `far`,
  `thickness`, `color`, `line_width`.

### mid_debris

![mid_debris](mid_debris/preview.png)

Low-poly wireframe rocks come at the camera, tumbling, and pass it on both sides. No rock comes
nearer than `clear` metres to the lane's middle or to the camera. In a drop, a kick bursts the
rock nearest 5 m ahead into shards, at most once every 2 bars. The shards fly out and shrink away
within a bar.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence` (towards 0 the rocks shrink away one after another), `count` (up to 64),
  `seed` (other places), `size` (a rock's mean size, metres), `speed` (times the floor's speed),
  `spread`, `clear`, `low`, `high` (the band of heights, metres), `near`, `far`, `spin` (turns per
  beat); the burst's `burst` (on or off), `every` (bars between bursts), `beats_per_bar`,
  `shard_beats` (how long the shards fly), `reach` (metres they fly out); `color`, `extent`.

### mid_fog

![mid_fog](mid_fog/preview.png)

Banks of fog ahead on the lane, drawn as glowing horizontal lines. The banks stand still over the
floor, so the camera flies through them. Drifting noise lights the lines, brighter with the bass.
A bank fades out shortly before the camera reaches it. The light is faint and adds to the picture,
so the banks need no sorting.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence` (towards 0 the banks fade one after another), `count` (up to 12), `seed`,
  `spread` (metres beside the lane), `low`, `high` (heights of the banks' middles), `sway`
  (sideways drift), `near`, `far`, `width`, `height` (a bank's size, metres), `brightness`,
  `quiet` (the light left without bass), `fade` (metres before the camera where a bank is gone),
  `start`, `end` (the distance fade; keep them at the stage's), `detail`, `thickness` (how much of
  a bank the fog fills), `cover` (how much it hides), `line_width`, `color`.
- The stage's distance fade goes by the render's data pass, which leaves out a surface under 5 %
  alpha. Keep `cover` under 0.05, or the fog takes the place of the lines behind it there. So the
  banks fade with distance by themselves, through `start` and `end` (2.5 and 7 m, nearer than the
  stage's fade: the banks shine brighter than a line, and far banks stood out as stripes over the
  horizon).

### mid_swarm

![mid_swarm](mid_swarm/preview.png)

A swarm of up to 256 wireframe darts flying ahead of the glider. Each dart is a boid: it keeps its
distance from its neighbours, matches their heading and keeps to their middle. The swarm follows a
leader that wanders along the lane. Through the build before a boundary the swarm draws together.
A drop scatters it. The darts stay above the floor, under the ceiling and 4.5 to 9 m ahead.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence` (towards 0 the darts shrink away one after another), `agents`, `at` (metres
  ahead), `hover` (metres up), `size` (a dart's length), `wander` (how far the leader strays
  sideways), `speed` (metres per second), `scatter` (how hard a drop throws them apart), `color`,
  `extent`.
- The swarm's state lives in a `tex.feedback`, so the flight carries on from frame to frame. After
  a seek the swarm flies on from where it was.

### mid_shapes

![mid_shapes](mid_shapes/preview.png)

Wireframe solids fly in a ring ahead of the camera, above the glider: octahedra, tori and
icosahedra. The ring turns on at every bar. Each solid makes a quick turn on every beat. Through
the build before a boundary the ring draws together. A drop throws the solids apart, and they
gather again within a few seconds. Towards `presence` 0 the solids shrink away one after
another, and the ring flies off ahead.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence`, `count` (3 to 7 solids), `seed` (which solid takes which shape), `size`
  (metres across), `at` (metres ahead), `hover` (metres up), `radius` (the ring's half width),
  `depth` (its half depth along the flight), `tilt` (how much higher its far side flies),
  `tight` (how much a build draws it together), `scatter` (how far a drop throws the solids),
  `turn` (the share of a step the ring turns on per bar), `spin` (turns per beat), `color`,
  `line_width`.
- The shapes come from `shapes.py` in the folder.

### mid_lasers

![mid_lasers](mid_lasers/preview.png)

A fan of laser beams. The beams start at one point low over the lane, where the floor fades
into the distance, and pass the camera above and on both sides. The fan swings from side to side
once every two bars. Every kick takes the beams out for one frame, so they flicker with the
beat. The fan suits a drop; `beats.arrange` decides when it shows. Towards `presence` 0 the
outer beams draw back into their point first. With the stage's `pose` wired, the point lies at
the stage's `axis`, and where the stage is a tube (`curve` 1) the fan opens all round the point:
the beams leave the tube's axis far ahead and pass the camera on every side.

- In: `bus`, `lane`, `pose` (the stage's; unwired, the point is `low` over the floor and the
  fan opens upwards). Out: `out` (into the stage's `mid`).
- Params: `presence`, `count` (5 to 15 beams), `origin` (metres ahead where the beams start),
  `low` (their height there), `reach` (metres ahead where they end; below 0 behind the camera),
  `across` (how far out the outer beams end), `up` (how high the middle beam ends), `open`
  (degrees from the middle to the outer beams), `sweep` (degrees the middle swings), `strobe`
  (on or off), `thickness`, `color`, `line_width`.

### mid_scope

![mid_scope](mid_scope/preview.png)

A green line in the air over the lane, like an oscilloscope. It is a Lissajous figure of the
music's levels, not its waveform. The bus carries levels only. Two sine waves draw the figure, one
across and one up and down. The mids set the frequency across and the treble the one up and
down. The bass makes the figure larger. Each beat turns the figure on by a step. The line is
closed and made of 192 short segments. Towards `presence` 0 the line shortens until it is gone.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence`, `segments` (16 to 256), `at` (metres ahead), `hover` (metres up),
  `across`, `tall` (half the figure's width and height, metres), `depth` (how far the line bends
  towards the camera), `grow` (how much the bass enlarges it), `kick` (turns per beat),
  `smooth` (seconds the levels take to fall), `thickness`, `color`, `line_width`.

### mid_equalisers

![mid_equalisers](mid_equalisers/preview.png)

Rings of equaliser bars on the wall of the tunnel, to fly through. A new ring comes every
`every` bars of travel. Each ring stands in the world and comes towards the camera, so the
camera flies through it on a downbeat. About five rings show in a row, the far ones fading in at
the tube's far end. Each bar stands on the wall and points towards the axis. Round a ring the
bars show the music as `sky_bars` does: the bass in the middle of the row, the mids beside it,
the treble at both ends. The row covers `arc` degrees of the ring, and the rest stays open. Each
ring is turned `turn` degrees against the one before, so the opening winds round the tube from
ring to ring. The bars jump up on each beat, and an onset lifts them.

The bars stay out of a round space of `open` metres round the flight, `middle` metres above the
tube's bottom. The bars near the bottom are short or missing there. The glider and the camera
pass through at every camera preset. Towards `presence` 0 the bars shrink into the wall. The
element needs a tube: where the stage's `curve` is 0 (the synthwave plane, the shaft) the bars
are gone.

- In: `bus`, `lane` (for `step`), `pose` (the stage's: `axis`, `radius`, `curve`, `far`). Out:
  `out` (into the stage's `mid`).
- Params: `presence`, `warmth`, `brightness`, `every` (bars from one ring to the next, 2),
  `turn` (degrees each ring turns against the one before, 20), `count` (bars round a ring, 8 to
  48, 32), `arc` (degrees of the ring the bars cover, 270), `cells` (cells a full bar is stacked
  of, 1 to 8; 1 draws solid bars), `tall` (metres a bar stands out of the wall at full level,
  1.6), `open` and `middle` (the free space: radius 2 m, its middle 2 m above the bottom), `gap`
  (dark share between two bars), `depth` (metres a cell is deep along the tube), `jitter` (how
  much each bar moves by itself), `kick` (the lift on an onset), `pump` (how far the bars fall
  over each beat, 0.4), `beats_per_bar`.

### mid_traps

![mid_traps](mid_traps/preview.png)

Laser traps in the tunnel: thin red beams, each from one point on the tube's wall to another.
Together they make a maze with one free corridor through it, and the glider flies along that
corridor (its flight 4). The beams stand in the world and come towards the camera with it, two
on every beat of travel. Each beam glows, flickers a little by itself and flares on the beat.

**The corridor.** The corridor's middle wanders across the tube. It stands at a knot on every
second downbeat (`corridor_bars`), left of the tube's middle at one knot and right at the next,
and up or down by up to `climb`. Between knots it glides over. No beam comes nearer to the
corridor's middle than `open` metres, so the glider passes untouched. Most beams pass close to
the corridor, the rest anywhere. The corridor depends only on the distance travelled and `seed`.
The glider reads the same numbers from the `corridor` output, so it needs nothing from the
picture.

**The camera** stays on its preset. A beam that would pass within `eye` metres of the line
straight ahead of the camera stays dark, so the camera never flies through a beam. It passes
close to some.

**Presence.** Towards 1 the beams come on from the far end of the tube towards the camera;
towards 0 they go dark from the camera outwards. Through the arrangement's fade the glider takes
the corridor before the near beams come on, and leaves it after they have gone. Where the
stage's `curve` is 0 (the synthwave plane, the shaft) the beams are dark.

**The tube's turns.** The beams stay put while the tube rolls with the lane or makes its whole
turns every `roll_bars` bars. The beams' ends stay on the wall, which is round, but the long lines
slide past them. A maze that turned with the tube would sweep its beams through the glider and the
camera. The beams also stand where the tube runs out into a gap.

- In: `bus`, `lane` (for `step`), `pose` (the stage's: `height`, `axis`, `radius`, `curve`,
  `far`). Out: `out` (into the stage's `mid`), `corridor` (channels `seed`, `corridor_bars`,
  `wander`, `climb`, `middle`: into the glider's `corridor`).
- Params: `presence`, `seed` (another number gives another corridor and other beams),
  `corridor_bars` (bars from one knot to the next, 2), `wander` (metres the corridor moves to
  either side, 1.2), `climb` (metres up and down, 0.5), `middle` (its middle height above the
  tube's bottom, 2 m), `open` (the corridor's free radius, 0.9 m), `eye` (the free radius round
  the camera's line, 0.6 m), `beams` (beams per slice, 1 to 4, 2), `spacing` (beats from one
  slice to the next, 1), `slant` (metres one end of a beam may lie ahead of the other, 1),
  `brightness`, `flicker` (0 to 1, 0.3), `color` (red), `thickness`, `line_width` (the beam's
  core, 2 px), `glow_width` (its glow, 7 px), `arrive` and `leave` (seconds the beams take to
  follow a rise and a fall of `presence`, 1 and 0.15), `beats_per_bar`.

### mid_rings

![mid_rings](mid_rings/preview.png)

Broad rings around the tower of `stage_ascent`, outside its wall, seen through the open walls.
Each ring is a band of 12 black segments with glowing edges, in the storeys' colours. The rings
stand in the world and pass downwards with the storeys as the camera climbs. Only some storeys
have one, halfway between two floors, and never two storeys in a row: two or three rings show at
a time. Each ring leans by up to `tilt` degrees and turns once in `orbit` beats. Half of them
turn one way and half the other, so they turn against each other and against the tower. The
edges brighten with the bass and flash on each beat.

Towards `presence` 0 the rings widen out into the distance and dim, so they fade into the sky.
They do the same where the tower lies down as a tube (the pose's `curve` towards 1), so they are
gone through a turn-up. The rings stay outside the wall, away from the glider and the camera.

- In: `bus`, `pose` (the stage's: `radius`, `curve`). Out: `out` (into the stage's `mid`).
- Params: `presence`, `warmth`, `brightness`, `storey` and `storey_beats` (as the stage's
  `ceiling` and `storey_beats`: 3 m, 4 beats), `chance` (how many storeys have a ring: 0.35;
  at 1 about every third), `inner` and `outer` (metres from the axis to the outer edge of the
  smallest and the largest ring: 7.5 and 10), `band` (metres a band is high, 0.35), `tilt`
  (degrees the most tilted ring leans, 10), `orbit` (beats a ring takes to turn once, 256), `pulse`
  (the flash on each beat, 0.5), `line_width`.

### ceiling_curtains

![ceiling_curtains](ceiling_curtains/preview.png)

Curtains of wireframe cloth hanging from the ceiling across the lane. A curtain hangs every 2
bars of travel, so one passes the camera on every second downbeat. Each curtain is a row of 25
strips, and a wave runs down each strip. The mids make the cloth swing wider. Over the lane the
strips hang 0.7 m, so the camera flies under them. Beside the lane they hang 2.4 m. The strips
are lines only, so flying through them hides nothing.

- In: `bus`, `lane`. Out: `out` (into the stage's `ceiling`).
- Params: `presence` (towards 0 the strips draw up into the ceiling one after another),
  `spacing` (bars between two curtains), `strips` (3 to 32), `pitch` (metres from strip to
  strip), `fill` (share of the pitch a strip covers), `short` (metres a strip hangs over the
  lane, at most 0.8), `long` (metres beside the lane), `open` (metres from the lane's middle
  where the strips grow longer), `foot` (metres over which they grow), `wave` (degrees the hem
  swings at full mids), `calm` (share of the swing left in quiet mids), `waves` (waves from top
  to hem), `flow` (waves per beat), `ceiling` (its height, 4 m), `near`, `far`, `beats_per_bar`,
  `smooth` (seconds the mids take to fall), `color`, `line_width`, `extent`.

### ceiling_shafts

![ceiling_shafts](ceiling_shafts/preview.png)

Shafts of light falling slanted from openings in the ceiling, beside the lane. A shaft stands
every bar of travel, by turns left and right of the lane, so one passes the camera on each
downbeat. Each shaft is a bright square in the ceiling and a cone of 16 rays, leaning outwards
and towards the camera. The rays grow dimmer towards the floor, and a ring marks where the light
meets it. Dust drifts down inside each cone and glints. The treble brightens the light. The
palette's warmth turns the light from warm white to cold blue. `shafts.py` in the folder writes
the models.

- In: `bus`, `lane`. Out: `out` (into the stage's `ceiling`).
- Params: `presence` (towards 0 the shafts draw up into the ceiling one after another),
  `spacing` (bars between two shafts), `size` (the cone's radius at the floor, metres), `narrow`
  (its radius at the ceiling, as a share of `size`), `slant` (degrees from straight down),
  `outwards` (degrees the lean turns from towards the camera to away from the lane), `lane` (1.6),
  `clear` (metres between the lane's edge and the nearest ray), `base` (the light without
  treble), `gain` (the light the full treble adds), `warmth`, `dust` (motes per shaft, 0 to 32),
  `mote` (a mote's size, metres), `fall` (how fast the dust sinks), `fade` and `gone` (metres
  ahead where the light starts to fade and where it is gone), `ceiling`, `near`, `far`,
  `beats_per_bar`, `smooth` (seconds the treble takes to fall), `line_width`, `extent`.

### ceiling_lights

![ceiling_lights](ceiling_lights/preview.png)

Two rows of paper lanterns under the ceiling, one on each side of the lane. Each lantern hangs on
a short string: a body of paper round an ellipsoid, filled in the lantern's colour, with 6 ribs
and 2 rings on it as darker lines, and a tassel. The lanterns swing
a little to and fro, more with the mids. A running light switches along each row: on every beat
the next lantern ahead lights up. The lanterns behind it glow on dimmer for a few beats. The light
runs away from the camera, faster than the floor comes towards it. The pattern repeats every 8
lanterns, and the right row runs half a pattern behind the left one. Each lantern of a pattern has
its own colour from the warm or cold palette; between warm and cold, each lantern takes one
palette whole, so the paper stays coloured. A dark lantern's paper glows dimly, a lit one's
shines bright and a little whiter. In a drop every lantern is on, and the
drop's flash lights them all up. `lantern.py` in the folder writes the models.

- In: `bus` (it reads `beat_index`, `section_role` and `drop`), `lane`. Out: `out` (into the
  stage's `ceiling`).
- Params: `presence` (towards 0 the lanterns draw up into the ceiling one after another),
  `spacing` (beats of travel between two lanterns), `period` (lanterns from one running light to
  the next), `trail` (dimmer lanterns behind it, 0 to 4), `apart` (metres from the lane's middle to
  each row), `size` (1 reaches 0.98 m down with its tassel; at most 1.2), `idle` (a dark lantern's
  paper), `full` (every lantern's light in a drop), `glow` (how bright a lit lantern's paper shines), `swing`
  (degrees at full mids, 7), `calm` (the share of the swing left without mids), `warmth`,
  `ceiling`, `near`, `far`, `line_width`, `extent`.

### sky_galaxy

![sky_galaxy](sky_galaxy/preview.png)

A spiral galaxy seen at a slant, over black, in the stage's palette: a warm core, arms of stars
with knots of glowing gas, dust lanes. It turns with `travelled` and brightens with the bass.

- In: `bus`. Out: `out`.
- Params: `presence` (towards 0 the galaxy dims to a dark sky), `brightness`, `warmth` (the
  palette, as on the stage), `spin` (degrees per beat), `lift`, `energy_curve`; its shape: `tilt`, `angle`, `size`,
  `twist`, `arms`, `bulge`, `dust`, `center`.

### sky_stars

![sky_stars](sky_stars/preview.png)

A night sky of stars over black, for `stage_valley`: many faint stars, fewer medium ones and a
few bright ones, in patches of denser and sparser sky. Venus is the brightest point, steady,
low over the horizon. The treble makes the stars twinkle. Towards the stage's `dawn` 1 the stars
near the horizon fade; Venus stays. The stars keep their place in the sky when the camera
moves.

- In: `bus`, `pose` (the stage's: the horizon, `far` and `dawn` come from it). Out: `out`.
- Params: `presence` (towards 0 the stars dim to a black sky), `brightness`, `seed` (another
  star field), `density` (1 the usual sky, 0 only the brightest stars), `size` (a star's size in
  pixels: 0.8), `twinkle` (1; 0 the stars stand still), `venus` (Venus's light, 1; 0 none),
  `venus_x` and `venus_y` (where Venus stands, in picture heights from straight ahead and above
  the horizon: -0.32 and 0.1), `thin` (how high above the horizon the dawn thins the stars:
  0.35 picture heights).

### sky_horizon_glow

![sky_horizon_glow](sky_horizon_glow/preview.png)

A second layer of the dawn for `stage_valley`, screened over the stage's band: broad soft rays
fan up from below the horizon, and a faint halo lies over the band. Both follow the stage's
swell. They are strongest in a breakdown and sink back to about a third (`calm`) on the drop. Towards
`dawn` 1 the rays reach higher and the halo grows wider. The glow has no disc: its light comes
from below the horizon, so it never becomes a sun. The rays wander slowly as the world travels.

- In: `bus`, `pose` (the stage's: the horizon, `swell` and `dawn` come from it). Out: `out`.
- Params: `presence`, `brightness`, `rays` (the rays' light, 1; 0 only the halo), `count`
  (about how many rays: 7), `tall` (how high the rays reach at swell and dawn 1: 0.45 picture
  heights), `halo` (1; 0 only the rays), `calm` (the light left in a full beat, when the swell
  is 0: 0.35), `warmth` (0 copper rays and an amber halo, 1 pale bronze rays and a jade halo),
  `seed`, `turn` (how fast the rays wander).

### sky_volcano

![sky_volcano](sky_volcano/preview.png)

A volcano far off to one side of the horizon of `stage_valley`, with a smoke plume. Its crater
glows like embers, and the bass makes them flicker a little. A drop makes the crater flare: the
glow, a few lava lines down the flank and the bottom of the plume brighten, then sink back over
about 3 seconds. The plume rises from the crater and leans away from the middle, lit from below.
The sky is screened, so the mountain cannot darken the stars or the band. A thin rim of dawn
light draws its outline instead, brighter on the side towards the band.

- In: `bus` (`drop`, the bass and `travelled`), `pose` (the horizon and `dawn`). Out: `out`.
- Params: `presence`, `brightness`, `place` (where it stands, in picture heights from straight
  ahead: 0.62, to the right; negative to the left), `tall` (its height: 0.09 picture heights),
  `wide` (the foot's half width in heights: 3.2), `crater` (the embers between drops), `lava`
  (the lava lines on a flare; 0 none), `plume` (0 no smoke), `rise` (how high the plume rises:
  0.3 picture heights), `wind` (how far it leans), `rim` (the outline's light), `warmth` (1
  turns the rim jade; the embers stay), `seed`, `sink` (seconds the flare takes to fall to a
  third: 2.5).

### sky_clouds

![sky_clouds](sky_clouds/preview.png)

A deck of clouds low over the horizon of `stage_valley`. The band lights the clouds from below:
their lower edges glow copper, most in the middle over the band, at dawn and in the swell.
Above, the clouds stay dark. The clouds crowd together towards the horizon, as a flat layer
seen from below does. The deck drifts slowly towards the camera as the world travels, and a
little sideways. The sky is screened, so the clouds cannot hide the stars or the band.

- In: `bus` (`travelled`), `pose` (the horizon, `swell` and `dawn`). Out: `out`.
- Params: `presence` (towards 0 the deck dims and thins out), `brightness`, `cover` (how much
  of the deck is cloud: 0.55), `low` and `top` (where the deck begins and ends above the
  horizon: 0.025 and 0.22 picture heights), `size`, `spread` (how far to the sides the band
  lights the clouds: 0.8 picture heights), `under` (the light on the undersides), `drift` and
  `wind` (per beat travelled, towards the camera and sideways), `warmth` (1 pale bronze edges
  and jade-tinted tops), `seed`.

### sky_sun

![sky_sun](sky_sun/preview.png)

The synthwave sun: a big disc standing on the floor's far edge, with dark lines across its lower
half that widen towards the bottom and drift slowly down. Below the horizon it shines through
the floor. A glow lies around it. The bass
brightens it and swells it a little, a drop makes it flash.

- In: `bus`, `pose` (the stage's: the horizon moves with the camera). Out: `out`.
- Params: `presence` (towards 0 the sun dims to a dark sky), `brightness`, `warmth` (0 yellow to
  magenta, 1 cyan to violet, as on the stage), `size` (radius, in picture heights of a 60 degree
  view), `height` (its centre over the horizon, in the same unit), `lines`, `glow`, `reach`
  (metres ahead where the floor ends: the stage's `end`, 10), `through` (how much of the sun
  shines through the floor below the horizon: 1 all of it, the default; 0 cuts it at the
  horizon).

### sky_aurora

![sky_aurora](sky_aurora/preview.png)

Curtains of light over the horizon: bands hang one above the other, each a bright wavy hem with
rays rising from it. They drift sideways and fold slowly as the world travels. The treble
brightens them.

- In: `bus`, `pose` (the stage's). Out: `out`.
- Params: `presence` (towards 0 the curtains dim to a dark sky), `brightness`, `warmth` (0 orange
  to magenta, 1 green to violet, as on the stage), `bands` (1 to 5), `height` (the lowest hem over
  the horizon, in picture heights of a 60 degree view), `speed` (per beat travelled), `glow`,
  `reach` (as on `sky_sun`).

### sky_planet

![sky_planet](sky_planet/preview.png)

A planet with a tilted ring over the horizon. Bands of cloud in the palette run around it and
drift as it turns. The planet grows over each track: small at the track's start, twice the size
and a little higher at its end. It turns slowly and holds still against the beat (`breathe` lets the bass swell it). The ring's near half passes in front
of the disc, its far half behind it.

- In: `bus` (it reads `track_phase`), `pose` (the stage's). Out: `out`.
- Params: `presence` (towards 0 the planet dims to a dark sky), `brightness`, `warmth`, `size`
  (radius at a track's start, in picture heights), `growth` (how much larger at a track's end: 1
  twice), `place` (across the sky), `height` (its centre over the horizon), `rise` (how much
  higher at a track's end), `tilt`, `open` (how far the ring opens towards the viewer), `bands`,
  `spin` (turns per beat), `breathe`, `reach` (metres ahead where the floor fades out: the
  stage's `end`, where the horizon lies), `bend` (bound to the pose's `curve`: in a tube the
  planet hangs smaller in the middle of its far end, inside the ring), `axis`, `radius`, `far` as
  on `sky_bars`.

### sky_bars

![sky_bars](sky_bars/preview.png)

A row of glowing bars on the horizon, like an equaliser: the bass in the middle, the mids beside
it, the treble at both ends. Each bar is a stack of cells and moves a little by itself. The bus
has three levels, not a spectrum, so the bars between the bands blend them. An onset lifts
every bar for a moment.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `count` (bars), `span` (half the row's width, in
  picture heights), `tall` (a bar at full level), `gap` (dark share between bars), `cells` (0
  draws solid bars), `jitter` (how much each bar moves by itself), `kick` (the lift on an onset),
  `glow`, `reach`, `bend` (0 a straight row; 1 the row wraps round the view's vanishing point as
  a ring, the bars pointing to its middle: bind it to the pose's `curve`, so in a tube the bars
  stand on the wall of its far end), `axis`, `radius`, `far` (where the tube's far end lies;
  bound to the pose's channels of the same names).

In the tunnel, [mid_equalisers](#mid_equalisers) can take the row's place: rings of bars that
come nearer and that the camera flies through.

### sky_city

![sky_city](sky_city/preview.png)

A skyline on the horizon: two rows of towers, the far one lower and dimmer, some with a mast and a
blinking light. The outlines glow in the palette. Windows light up and go out, and the louder the
mids, the more are lit. The skyline slides slowly sideways as the world travels, the far row at
half the pace. The towers' bodies only glow faintly, so the nebula shows through them a little.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `seed` (another number, another skyline), `tall`
  (the tallest towers, in picture heights), `slide` (picture heights per beat), `windows` (share
  lit in quiet passages), `flicker` (how much the mids light more and how often windows change),
  `edge` (the outlines' light), `reach`, `bend`, `axis`, `radius` and `far` as on `sky_bars` (the skyline wraps
  round the tube's far end, the towers pointing to its middle).

### sky_figure

![sky_figure](sky_figure/preview.png)

A huge figure of lines standing on the horizon. It stands still, and on every second bar it
sways with one arm out. Its arms rise through a build and are up through a drop, and the drop's
flash makes it jump. It bobs on the beats.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall` (its height standing, in picture heights;
  arms up reach a fifth higher), `place` (across the sky), `thickness` (pixels), `glow`, `reach`
  (as on `sky_planet`).

The six figures below are variants of `sky_figure`. Each takes the same poses on the same channels, so any of them can stand in its place in the sky slot. Each also stands in the tunnel: bind `bend` to the pose's `curve` (the component does) and the figure stands smaller on the bottom of the tube's far end (from the pose's `axis`, `radius` and `far`).

### sky_figure_solid

![sky_figure_solid](sky_figure_solid/preview.png)

A variant of `sky_figure`: a filled low-poly silhouette. Each limb and the trunk are a few flat facets of dark fill, lit from the upper left, inside a bright edge.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall`, `place`, `tube` (its height in the tunnel, as a share of the tube's far end), `thickness` (the edge, pixels), `fill` (the facets' light), `seams` (the lines between facets), `glow`, `reach`.

### sky_figure_robot

![sky_figure_robot](sky_figure_robot/preview.png)

A variant of `sky_figure`: a robot of boxes. Every part is a wireframe box with a black inside, so a box in front hides the lines behind it. Two eyes glow in the head, and the antenna's tip flashes on each beat. It moves in steps: the arms in quarters, the sway at once with the bar, a drop of the body on each beat.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall`, `place`, `tube` (its height in the tunnel, as a share of the tube's far end), `thickness`, `depth` (how far the back faces lie), `eyes`, `glow`, `reach`.

### sky_figure_dancer

![sky_figure_dancer](sky_figure_dancer/preview.png)

A variant of `sky_figure`: a slim dancer with a cloak of lines. The lines hang from the shoulders, the arms and the wrists. They swing with the mids, the outer ones a little later, so the cloak trails the arms. With the arms up the cloak opens like wings.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall`, `place`, `tube` (its height in the tunnel, as a share of the tube's far end), `thickness`, `strands` (lines per side), `cloak` (their light), `hem` (the cloak's lower edge), `swing`, `glow`, `reach`.

### sky_figure_joints

![sky_figure_joints](sky_figure_joints/preview.png)

A variant of `sky_figure`: glowing joints and thin faint bones, like a figure in Tron. On each beat a pulse runs from the neck along the bones to the hands and feet, and each joint flashes as it passes.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall`, `place`, `tube` (its height in the tunnel, as a share of the tube's far end), `thickness` (the bones), `dots` (a joint's radius), `bones` (their light), `pulse`, `glow`, `reach`.

### sky_figure_giant

![sky_figure_giant](sky_figure_giant/preview.png)

A variant of `sky_figure`: a giant seen from below. The legs are huge, and the body narrows upwards to a small head. The silhouette is filled, with dark bands across its lower part like the sun's, rolling slowly down.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall`, `place`, `tube` (its height in the tunnel, as a share of the tube's far end), `thickness`, `depth` (how much narrower the top is), `spread` (the feet's width), `lines` (bands), `glow`, `reach`.

### sky_figure_bust

![sky_figure_bust](sky_figure_bust/preview.png)

A variant of `sky_figure`: only head and shoulders, huge over the horizon, as a wireframe of latitude and longitude lines with glowing eyes and a mouth. The poses become the head's: it tilts to the side on the sway, lifts through a build, tilts far back in a drop and nods on each beat. The drop's flash makes the whole bust jump.

- In: `bus`, `pose`. Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `tall`, `place`, `tube` (its height in the tunnel, as a share of the tube's far end), `thickness`, `rim` (the light where a surface turns away), `fill`, `eyes`, `reach`.

### sky_eyes

![sky_eyes](sky_eyes/preview.png)

Pairs of eyes in the dark sky. Each eye is a glowing almond with an iris in the palette. The
pupils follow the lane and look the way it turns. Now and then a pair blinks: shut for about four
frames, on a bar that its own number picks. The eyes suit breaks and intros, but the arrangement
treats them like any sky element.

- In: `bus`, `pose`, `lane` (the stage's; without it the pupils swing with `drift`). Out: `out`.
- Params: `presence`, `brightness`, `warmth`, `pairs` (1 to 6), `seed` (where the pairs sit and
  when they blink), `size` (an eye's half width, in picture heights), `look` (how far the pupils
  move), `blinks` (share of bars with a blink), `reach`.

### outside_ships

![outside_ships](outside_ships/preview.png)

Huge ships far outside the tower of `stage_ascent`, seen through its open walls. Each ship is a
wireframe hull with a black body, engines glowing at its stern and running lights that blink:
red on the left, green on the right, white strobes, red beacons and amber lights. Three hulls
take turns: a hauler (a spine with cargo blocks), a wedge with two engine pods, and a ring ship.

**Crossing.** Each of `ships` lanes sends one ship at a time. A ship takes `cross` beats from
far out on one side to far out on the other, and is in view for about 40 % of them: about a
minute at 130 BPM with the defaults. Then its lane rests for `rest` beats. Each ship takes its
own hull, distance (`near` to `far` metres), height, heading and length (around `length`).
`seed` gives other ships.

**Parallax.** The tower's storeys pass down at the full climb. The ships sink by only `sink`
times the climb, and the nebula not at all. So the ships hang far behind the tower and drift
slowly against it.

**Presence.** Towards `presence` 0 the ships fade into the sky: edges, engines and lights dim,
and the black bodies turn see-through. They do the same where the tower lies down as a tube
(the pose's `curve` towards 1), so they are gone through a turn-up.

- In: `bus`, `pose` (the stage's: `curve`). Out: `out` (into the ascent's `outside`).
- Params: `presence`, `warmth`, `brightness`, `ships` (lanes, 1 to 6, 4), `cross` (beats a
  crossing takes, 384), `rest` (beats a lane waits, 256), `near` and `far` (metres to the
  ships' paths, 150 and 340), `length` (metres a ship is long on average, 200), `sink` (share
  of the climb the ships sink by, 0.15), `storey` and `storey_beats` (as the stage's `ceiling`
  and `storey_beats`), `blink` (beats from one flash of a light to the next, 2), `seed`,
  `line_width`.

### outside_megastructure

![outside_megastructure](outside_megastructure/preview.png)

A structure far larger than the tower of `stage_ascent`, which the climb passes through for a
stretch. Two parts, both wireframe edges on black bodies:

- **The lattice** stands just outside the tower's wall. Every 12 m of climb an octagonal collar
  of box girders rings the tower, with heavy nodes at its corners. Lattice columns join the
  collars, and two long trusses run out from each collar into the distance. The lattice passes
  down with the storeys and turns with the tower. It stays 7 m or more from the tower's axis,
  away from the glider and the camera.
- **The ring** stands far ahead: a band of 32 segments 1.3 km across, a lattice ring inside it
  and four trusses in towards a glowing core with a corona. It turns slowly and hangs above the
  glider, seen through the open walls.

**Coming and going.** As `presence` rises, the lattice comes down from above and the ring drops
into view from over the top of the picture. As `presence` falls, both sink away below. The
movement follows the presence, so a longer fade gives a slower arrival. The edges dim and the
bodies turn see-through on the way. They do the same where the tower lies down as a tube (the
pose's `curve` towards 1), so they are gone through a turn-up.

**Layers.** `near` (the lattice) goes into the stage's `mid`. There it hides and is hidden by
the storeys and posts as it should, and fades with distance like them. `out` (the ring and
core) goes into the ascent's `outside`. There it stays sharp 1.8 km away.

- In: `bus`, `pose` (the stage's: `radius`, `curve`). Out: `near` (into the stage's `mid`),
  `out` (into the ascent's `outside`).
- Params: `presence`, `warmth`, `brightness`, `storey` and `storey_beats` (as the stage's
  `ceiling` and `storey_beats`: the lattice climbs with the storeys), `orbit` (as the stage's:
  the lattice turns with the tower, 128), `distance` (metres to the ring, 1800), `height`
  (metres the ring's middle stands above the camera, 380), `drop` (metres above the lattice
  starts as it comes in, 40), `far_drop` (the same for the ring, 2400), `spin` (beats the ring
  takes to turn once, 2048), `pulse` (the flash on each beat, 0.5), `line_width` (the lattice's
  lines).

### post_scanlines

![post_scanlines](post_scanlines/preview.png)

Dark lines across the picture, like an old monitor. The lines roll slowly down as the world
travels. On every onset of the music they darken for a moment, so they flicker with the beat.
The preview is a piece of the picture at full size; scaled down, the lines vanish.

- In: `in` (the picture, e.g. the stage's `out`), `bus`. Out: `out`.
- Params: `presence` (0 passes the picture on unchanged), `pitch` (pixels from one line to the
  next), `strength` (how dark a line is: 0 none, 1 black), `thickness` (share of the pitch a line
  covers), `roll` (lines the pattern moves per beat travelled), `flicker` (how much darker lines
  and picture get on an onset), `decay` (seconds the flicker takes to fade).

### post_hud

![post_hud](post_hud/preview.png)

A head-up display over the picture, in thin lines of the palette's colour. A crosshair sits in
the middle. The horizon and a pitch ladder lie where the stage's floor ends, and tilt with the
camera. A tape on the right reads the camera's height. Top left the bar count and a box per
beat. Bottom left a radar: the sweep turns once per bar, each section of the track is a blip,
and the rim fills as the track goes on. The lines dim and drop out for a moment on an onset and
flash on a drop. They add light to the picture and cover nothing. Chain it last, after
`post_scanlines`, so the lines stay sharp.

- In: `in` (the picture), `bus`, `pose` (the stage's `pose`; unwired, the horizon stands where
  the first camera has it). Out: `out`.
- Params: `presence` (0 passes the picture on unchanged), `warmth`, `brightness`, `thickness`
  (line width in pixels of a 1080 line picture), `flicker` (how much an onset dims the lines),
  `decay` (seconds the flicker takes to fade), `reach` (metres to the floor's end, as the stage's
  fade), `scale` (picture heights of the tape per metre), `beats_per_bar`.

### room_lights

![room_lights](room_lights/preview.png)

The lights of the room around the screen, from the same arrangement as the stage. The
`pattern` output is a picture for the room's columns: one band per column, side by side, from
the floor at the bottom to the top. The `colours` output holds channels for DMX lights. Both
use the same two colours: an accent and a deep colour. `warmth` moves them from warm (orange and
crimson) through pink and purple at 0.5 to cold (cyan and blue) at 1.

The picture shows one of three patterns, picked by `pattern`:

- 0 stripes: stripes rise slowly up the columns, neighbouring columns half a stripe apart. They
  widen over each 8-bar phrase.
- 1 pulse: every column flashes on the beat, brightest at the floor. Neighbouring columns and
  every other beat take turns between accent and deep colour. The first beat of a phrase is
  nearly white.
- 2 chase: one column lights up after the other, one step per beat, with a trail. The chase
  turns round at each phrase. In the second half of a build it steps twice per beat.

A change of `pattern` crossfades over about a second. `beats.arrange {lights: "/room"}` writes
a `pattern` column: stripes in an intro, an outro and a break, the chase in a build, the pulse
on a drop, chase and pulse by turns from track to track in a main section. It also binds
`warmth` and `intensity`, so the room dims and changes colour with the stage.

**The `colours` channels** (all 0 to 1):

| channel | what |
|---|---|
| `red`, `green`, `blue` | the accent colour |
| `red2`, `green2`, `blue2` | the deep colour |
| `level` | a dimmer: `intensity`, pumping on the beat between a quarter and all of it |
| `beat` | 1 on the beat, falling to 0 before the next one |
| `warmth`, `intensity` | as the arrangement gives them |
| `build`, `drop`, `phrase` | from the music bus: the rise before a section, the flash of a drop, the way through the phrase |

For a light with dimmer and colour, pick its slots in order with a `chan.select` (`level red
green blue`) and wire that into an `out.dmx`. [DMX](../docs/dmx.md#lights-from-a-music-stage)
shows the whole route.

- In: `bus`. Out: `pattern` (picture), `colours` (channels).
- Params: `pattern` (0 to 2, above), `warmth`, `intensity`, `columns` (bands in the picture, 1
  to 16, default 4), `bands` (stripes up a column), `rise` (stripes the pattern rises per beat
  travelled), `tail` (columns the chase's light trails behind it), `snap` (how fast the beat
  flash falls: a power of the beat, default 3), `patterns` (a csv table, one row per pattern:
  the shares of `stripes`, `pulse` and `chase`; shares between mix them), `pattern_glide`
  (seconds to cover 63 % of a change of pattern, default 0.4), `width` and `height` of the
  picture (1024 x 512). The colours at `warmth` 0, 0.5 and 1 are the `chan.constant` nodes `warm`, `mid` and `cold`
  inside.

### previs_room

![previs_room](previs_room/preview.png)

A room seen from the audience, for judging a show without standing in one. The main wall shows
the `main` picture through a projector, 9.6 x 5.4 m. Four columns stand beside it, two on each
side, and show the bands of the `pattern` picture: column 1 on the left takes the left band,
floor at the bottom. Four wash lights take the `colours` channels: two from the ceiling in the
accent colour, dimmed by `level`; two from the floor up the side walls in the deep colour,
dimmed by `intensity`.

Wire it to a music stage and its `room_lights`: `main` from the stage's finished picture,
`pattern` and `colours` from `room_lights`. The room renders only while something takes its
picture: the viewer, or an `out.video` while it records. An `out.video` that does not record
leaves it alone, so the room costs nothing in a show.

- In: `main` (picture), `pattern` (picture, four bands side by side), `colours` (channels as
  `room_lights` gives them). Out: `out` (picture).
- Params: `width` and `height` of the picture (1280 x 720), `eye` and `look_at` (where the
  viewer stands and looks, in metres, z up), `fov` (55 degrees), `brightness` (the main wall's
  projector, 1), `wash` (the ceiling lights' level times this, 1.5), `deep` (the floor lights'
  intensity times this, 1), `ambient` (0.04), `walls` and `floor` (their colours).
- The room is 16 m wide, 12 m deep and 6 m high, the main wall at y = 2. The four columns are
  0.7 m wide and 5.2 m high, at x = -6.9, -5.7, 5.7 and 6.9. `room_lights` with `columns` other
  than 4 does not fit them.
