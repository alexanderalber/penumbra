# projection-room

A room built in 3D, to try projection before standing in the real room.

- Geometry (`geo.primitive`, `geo.transform`, `geo.merge`) makes the walls, floor and props;
  `geo.material` gives them materials (`mat.pbr` for gold, chrome and the floor, `mat.phong`
  for the matte walls, a shader texture for the stained glass pane).
- `/projector` is a `light.projector`: it throws the picture `/content` into the room from the
  camera `/beamer`. `/sun_spot` and `/window_sun` are spot lights with shadows, `/around` lights
  the room from its surroundings (`surroundings.wgsl`), which chrome and gold reflect.
- `/view` renders the room from `/eye`, with ambient occlusion; `/through_beamer` shows what
  the projector's lens sees.

Try: change `/content_title`'s text, move `/beamer`, or switch the viewer to **free** and look
around (`docs/navigation.md`).
