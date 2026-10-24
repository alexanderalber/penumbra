# A ball bouncing in a box under gravity. The state lives in module globals, which keep their
# values from one frame to the next. Edit and save: the node reloads (and the ball starts over).

PARAMS = {
    "gravity": (1.5, 0.0, 5.0),
    "bounce": (0.85, 0.0, 1.0),
}

x, y = 0.2, 0.2
vx, vy = 0.35, 0.0
last = None
bounces = 0


def cook(cx):
    global x, y, vx, vy, last, bounces
    t = cx.time
    # Seconds since the last frame; 0 on the first one and when the show clock jumps back.
    dt = 0.0 if last is None or t < last else min(t - last, 0.1)
    last = t

    vy += cx.params["gravity"] * dt
    x += vx * dt
    y += vy * dt
    if x < 0.05 or x > 0.95:
        vx = -vx
        x = min(max(x, 0.05), 0.95)
        bounces += 1
    if y > 0.9:
        y = 0.9
        vy = -abs(vy) * cx.params["bounce"]
        bounces += 1
        if abs(vy) < 0.3:
            vy = -1.4  # nearly at rest: throw it up again (to a quarter from the top)

    # Position in image sizes (0, 0 top left), and as tex.transform's translate (from the centre).
    cx.out["x"] = x
    cx.out["y"] = y
    cx.out["tx"] = x - 0.5
    cx.out["ty"] = y - 0.5
    cx.out["bounces"] = bounces
