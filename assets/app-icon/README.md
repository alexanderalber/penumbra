# App icon

A sphere lit from the lower left, its soft terminator the penumbra.

- `penumbra.svg`: the sphere in a ring on a dark disc. The icon in `penumbra.exe` and the window
  icon on a dark taskbar or when the taskbar's colour is unknown.
- `penumbra-light.svg`: the sphere alone with a dark rim, the window icon on a light taskbar.

The app reads the taskbar's colour once at start. The PNGs (256 px) and `penumbra.ico` (16 to
256 px) are rendered from the SVGs at 1024 px and scaled down; re-render them after changing
an SVG.
