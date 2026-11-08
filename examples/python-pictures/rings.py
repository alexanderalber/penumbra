# Coloured rings from two centres that circle each other, drawn with numpy as a picture of
# 256 x 256 pixels. Edit and save: the node reloads.

import numpy as np

PARAMS = {
    "rings": (14.0, 2.0, 40.0),
    "speed": (0.6, 0.0, 3.0),
}

SIZE = 256
# Pixel centres from -0.5 to 0.5; row 0 is the top of the picture.
y, x = (np.mgrid[0:SIZE, 0:SIZE] + 0.5) / SIZE - 0.5


def cook(cx):
    t = cx.time * cx.params["speed"]
    k = cx.params["rings"] * np.pi
    a = np.sin(k * np.hypot(x - 0.2 * np.sin(t), y - 0.2 * np.cos(t)))
    b = np.sin(k * np.hypot(x + 0.25 * np.cos(0.7 * t), y + 0.2 * np.sin(0.9 * t)))
    v = (a + b) * 0.25 + 0.5
    rgb = np.stack([v, v * v, 1.0 - v], axis=-1)
    # uint8 is taken as sRGB, 0 to 255, like a PNG.
    cx.picture = (rgb * 255).astype(np.uint8)
