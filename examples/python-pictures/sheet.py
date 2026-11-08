# A sheet of 80 x 80 vertices with waves running over it, built with numpy as a mesh. The
# triangles stay the same; only the heights change each frame. Edit and save: the node reloads.

import numpy as np

PARAMS = {
    "height": (0.15, 0.0, 0.5),
    "waves": (3.0, 0.5, 8.0),
}

N = 80
# u runs left to right, v top to bottom, both 0 to 1: also the texture coordinates.
u, v = np.meshgrid(np.linspace(0.0, 1.0, N), np.linspace(0.0, 1.0, N))
UVS = np.stack([u, v], axis=-1).reshape(-1, 2)
# Two triangles per grid cell, as indices into the N * N vertices.
i = np.arange(N * N).reshape(N, N)
a, b, c, d = i[:-1, :-1], i[:-1, 1:], i[1:, :-1], i[1:, 1:]
INDICES = np.concatenate([np.stack([a, c, b], -1), np.stack([b, c, d], -1)]).reshape(-1, 3)
# 3 m wide, z up; v = 0 is the far edge.
X = (u - 0.5) * 3.0
Y = (0.5 - v) * 3.0


def cook(cx):
    t = cx.time
    w = cx.params["waves"] * np.pi
    z = cx.params["height"] * (np.sin(w * u + t) + np.cos(0.8 * w * v - 1.3 * t))
    positions = np.stack([X, Y, z], axis=-1).reshape(-1, 3)
    # Without `normals`, penumbra computes smooth ones from the shared vertices.
    cx.mesh = dict(positions=positions, indices=INDICES, uvs=UVS)
