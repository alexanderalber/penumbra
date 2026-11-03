# The content library

Ready-made effects, one component each. Most are content for scanned objects. They take what a
scan leaves behind (`scan.scene`'s and `scan.regions`' pictures, camera stills), never the face or
region numbers of one particular scene, so they fit any scanned scene. The last group is a stage
for music: a synthwave world that a `chan.beat_grid` drives, with elements to swap in and out.

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

A synthwave world that moves with a sound file: one stage, and elements that stand in it. They
all take the same music bus, the whole output of a `chan.beat_grid`. A project has one
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
  `lane` and give geometry, as floor elements do. Their output goes into the stage's `ceiling`.
  Towards `presence` 0 they draw up into the ceiling.
- **Sky elements** draw a picture over black. Their output goes into the stage's `sky`, screened
  over the nebula. Several go through a `tex.composite` with `op: screen`.
- **Post elements** change the finished picture. Each takes a picture in `in` and `bus`, and
  gives the changed picture in `out`. Chain them after the stage's `out`: stage, then
  `post_scanlines`, then the next one, then the output. At `presence` 0 a post element passes the
  picture on unchanged.
- **`presence`** (0 to 1) brings an element in and takes it out again. A floor element sinks into
  the floor towards 0, a mid element flies away, a sky element dims. To swap two elements, ramp
  one's `presence` down while the other's goes up, for example through the blend between two
  sections.
- **`beats.arrange`** does those swaps for a whole sound file. It writes a table with a row per
  section (the `presence` of each element, `warmth`, `intensity`) and binds the elements to it.
  Edit the table to change a section's picture. [An arrangement from the
  sections](../docs/audio.md#beats-of-a-sound-file) explains the rules. Name the slots `floor`,
  `ceiling`, `mid`, `sky` and `post`. `mid` and `sky` stay on through a break; `floor` and
  `ceiling` empty. In `post` every element is on or off by itself, so several can be on at once:
  they are off in an intro, outro and break and come on at a drop.
- **The camera** takes one of six presets of the stage. The arrangement's `camera` column picks
  one per section, and the stage glides to it.
- **Two stages.** A project can hold several stages side by side, each with its own floor, sky
  and ceiling elements. Wire their `out` into a `tex.switch`, or a `tex.cross` for two stages.
  Give `beats.arrange` the stages as a list and the switch's param:
  `stage: ["/stage_a", "/stage_b"], switch: "/stages.index"`. The `stage` column then changes at
  a strong track start, at most once in 8 minutes. A `tex.cross` (`switch: "/stages.cross"`)
  fades between the two stages through the row's fade; a `tex.switch` cuts half way through it.
  Wire the mid and post elements into every stage, or after the switch.

### stage_synthwave

![stage_synthwave](stage_synthwave/preview.png)

The world: a camera flying over a wireframe floor, under a wireframe ceiling, towards a nebula
in a warm or cold palette. The world moves one grid cell per beat of `travelled`. The lane
zigzags to the other side every two bars, each turn starting on a downbeat. The bass brightens
the nebula. Lines fade out with distance from the camera. The glider is the element
`mid_glider`.

- In: `bus`, `floor` (geometry standing on the grid), `mid` (geometry in front of the camera,
  such as the glider), `ceiling` (geometry hanging from the ceiling grid), `sky` (a picture
  screened over the nebula).
- Out: `out` (the picture), `lane` (256 x 1: per texel i, -8 + 0.25 i metres ahead, red the
  lane's x in metres, green its sideways slope per metre, blue `step`, alpha the floor: 1 where
  it stands, 0 in a gap).
- Params: `warmth` (0 the warm palette, 1 the cold one), `lift` (added to the bass, e.g. before a
  change), `energy_curve`, `dim` (the nebula's light); the lane's `bars_per_turn`, `swing`, `turn`,
  `beats_per_bar`; `step` (metres per beat); `start`, `end` of the distance fade.
- Camera: `camera` (the preset, 0 to 5), `glide` (seconds the camera takes to reach a new preset:
  most of the way after about 3 times this), `presets` (a csv table, one row per preset: `height`
  in metres, `pitch` in degrees up, `roll` in degrees, `fov` in degrees, `side` in degrees to the
  right). The presets: 0 low flight at 1.5 m, 1 higher and looking down for a build, 2 high and
  wide for a drop (2.9 m, 74 degrees), 3 still and level for a break, 4 and 5 two tilted variants
  that the tracks take in turn. `pitch` and `side` stay good to about 30 degrees.
- Gaps: `gap` (the share of the floor grid that breaks off, 0 to 1; 0 keeps it whole),
  `gap_length` (metres a gap runs, rounded to whole bars). A gap begins where a bar begins, so its
  near edge passes the camera on a downbeat. The lane floats on over nothing. Floor elements read
  the gaps from `lane` and sink there too. A gap that opens or closes fades. `beats.arrange`
  binds `gap` to its `gaps` column: 0.3 in a build, 0.6 in a break.

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
- Params: `presence` (towards 0 the peaks sink into the floor one after another), `seed`,
  `height` (the tallest peaks, metres; the camera is 2 m up, the ceiling 4 m), `width` (a peak's
  foot across), `length` (along the lane, as a multiple of `width`), `spacing` (between peaks),
  `peaks` (metres between summits: smaller is more jagged), `valleys` (peaks lower than this share
  of `height` are left out, so the floor shows between ridges), `foot`, `lane` (as on
  `floor_blocks`, 1.6), `spread` (the range's width, both sides), `near`, `far`, `color`, `extent`.

### floor_pillars

![floor_pillars](floor_pillars/preview.png)

Tall wireframe pillars in lines on both sides of the lane, like an avenue. A pillar stands every
`spacing` bars of travel, so the pillars pass the camera on downbeats. The lines follow the lane's
zigzag at a fixed distance from its edge. On each beat the pillars stretch a little.

- In: `bus`, `lane`. Out: `out`.
- Params: `presence` (towards 0 the pillars sink into the floor one after another), `spacing`
  (bars between two pillars), `height` (metres; the camera is 2 m up, the ceiling 4 m), `width`,
  `rows` (lines on each side, 1 to 3), `gap` (metres between lines), `pulse` (how far they stretch
  on a beat), `lane` (as on `floor_blocks`, 1.6), `near`, `far`, `beats_per_bar`, `color`,
  `extent`.

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

### mid_glider

![mid_glider](mid_glider/preview.png)

The glider: a wireframe hover craft flying ahead of the camera along the lane. It turns with the
lane, leans into each turn and bobs slowly. Towards `presence` 0 it climbs away ahead and fades
into the distance; at 0 it is gone. In a break `beats.arrange` keeps it at 0.6: about 4 m further
ahead and 0.6 m higher, small against the sky.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
- Params: `presence`, `at` (metres ahead of the camera), `bank` (how far it leans into a turn),
  `bob` (metres up and down), `scale` (0.16 makes it 2.4 m long), `hover` (metres above the
  floor).

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
- The stage's distance fade goes by the render's data pass, which leaves out a surface this
  faint. So the banks fade with distance by themselves, through `start` and `end`.

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
outer beams draw back into their point first.

- In: `bus`, `lane`. Out: `out` (into the stage's `mid`).
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

### sky_galaxy

![sky_galaxy](sky_galaxy/preview.png)

A spiral galaxy seen at a slant, over black, in the stage's palette: a warm core, arms of stars
with knots of glowing gas, dust lanes. It turns with `travelled` and brightens with the bass.

- In: `bus`. Out: `out`.
- Params: `presence` (towards 0 the galaxy dims to a dark sky), `brightness`, `warmth` (the
  palette, as on the stage), `spin` (degrees per beat), `lift`, `energy_curve`; its shape: `tilt`, `angle`, `size`,
  `twist`, `arms`, `bulge`, `dust`, `center`.

### sky_sun

![sky_sun](sky_sun/preview.png)

The synthwave sun: a big disc standing on the floor's far edge, with dark lines across its lower
half that widen towards the bottom and drift slowly down. A glow lies around it. The bass
brightens it and swells it a little, a drop makes it flash.

- In: `bus`. Out: `out`.
- Params: `presence` (towards 0 the sun dims to a dark sky), `brightness`, `warmth` (0 yellow to
  magenta, 1 cyan to violet, as on the stage), `size` (radius, in picture heights), `height` (its
  centre over the horizon, in picture heights), `lines`, `glow`, `horizon` (where the floor ends,
  0.68 of the picture's height with the stage's default fade; move it with the stage's `end`).

### sky_aurora

![sky_aurora](sky_aurora/preview.png)

Curtains of light over the horizon: bands hang one above the other, each a bright wavy hem with
rays rising from it. They drift sideways and fold slowly as the world travels. The treble
brightens them.

- In: `bus`. Out: `out`.
- Params: `presence` (towards 0 the curtains dim to a dark sky), `brightness`, `warmth` (0 orange
  to magenta, 1 green to violet, as on the stage), `bands` (1 to 5), `height` (the lowest hem over
  the horizon, in picture heights), `speed` (per beat travelled), `glow`, `horizon` (as on
  `sky_sun`).

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
