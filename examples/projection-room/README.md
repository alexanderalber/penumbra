# projection-room

A room built in 3D, to try projection before standing in the real room.

- Geometry (`geo.primitive`, `geo.transform`, `geo.merge`) makes the walls, floor and props;
  `geo.assign_material` gives them materials (`mat.pbr` for gold, chrome and the floor, `mat.phong`
  for the matte walls, a shader texture for the stained glass pane).
- `/projector` is a `light.projector`: it throws the picture `/content` into the room from the
  camera `/beamer`: drifting noise coloured through the `magma` palette (`/content_look`), with
  the title on top. `/sun_spot` and `/window_sun` are spot lights with shadows. `/window_sun`
  stands outside the room: it shines through a window in the side wall and the stained glass pane,
  which throws its colours onto the back wall. `/around` lights the room from its surroundings
  (`surroundings.wgsl`), which chrome and gold reflect.
- `/view` renders the room from `/eye`, with ambient occlusion; `/through_beamer` shows what
  the projector's lens sees.

Try: change `/content_title`'s text or `/content_look`'s palette, move `/beamer`, or switch the viewer to **free** and look
around (`docs/navigation.md`).
