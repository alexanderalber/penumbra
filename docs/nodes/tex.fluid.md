## How it works

A 2D fluid after Stam's Stable Fluids, simulated on the GPU every show frame. Forces come in first
(the `force` picture, the `splats` table, and `vorticity`, which brings back small swirls), then the
velocity is carried along by itself, thickened by `viscosity` when that is above 0, and made free of
divergence by `iterations` pressure steps. Last, the dye is carried along by the result.

The velocity lives on a coarse grid, `size` pixels wide; the dye has the output's size. So `size`
sets how fine the swirls get and what it costs, while `width` and `height` set only how sharp the
dye looks.

## Time and state

Velocity, dye and pressure are kept from frame to frame. Steps follow the show clock: a paused show
stands still, a frame a late show skipped becomes one longer step (at most 0.05 s, so a stall does
not simulate the time in between), and time running backwards is one frame's step. A new `size` or
output size carries the fluid over, resampled. With `persist` on, velocity and dye go into the state
snapshots, so a restart carries on where it stopped; `reset`, `node.reset` or R start from a still
fluid without dye.

## Typical networks

- Camera motion: `tex.video_in` into `tex.optical_flow`, its output into `force` as it is. People
  moving in front of the camera stir the fluid.
- Tracked blobs: `tex.blob_track`'s table into `splats` as it is (columns `x`, `y`, optionally `vx`,
  `vy`, `r`, `g`, `b`, `radius`; at most 64 rows).
- A mask into `obstacles` (solid where brighter than half) makes the fluid flow around a shape;
  `edges` decides whether the picture's border is a wall or lets the fluid out.
- The `velocity` output into `tex.displace` (midpoint 0) moves any picture along the flow.
