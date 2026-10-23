## What it is for

Normally a picture flows one way, from left to right through the chain, and every frame starts
from scratch. A feedback loop hands each frame's result back to the start of the chain, so the
next frame builds on it. That is how trails, echoes, smears and patterns that grow by themselves
come about: whatever the chain does to a frame (move it a little, dim it, shift its colour)
happens again and again, and adds up.

## Wiring it

1. Put a `tex.feedback` at the start of the chain and wire the picture to start from into
   `initial` (a black `tex.constant` is the usual choice).
2. Build the chain from the feedback's output: what should happen to the last frame each time.
3. Wire the end of the chain back into the feedback's `previous`. That wire is drawn dashed: it
   carries the previous frame, so it may run backwards without making a cycle.

Until the end of the chain has made a first frame, and while `reset` is on, `initial` is passed
on. R, the reset button or `node.reset` clear the loop the same way.

## Keeping it in check

Every step repeats every frame, so small values matter: a `tex.level` with `brightness` 0.96
lets a trail die out within a few seconds, at 1 nothing ever fades and the picture fills up. A
`tex.transform` with `scale` 1.01 zooms the trails slowly outwards. Mix new content in near the
end of the chain (a `tex.composite` with the content as `layer2`), or the loop has nothing to
work on but `initial`.

With `persist` on (the default), the loop's last frame goes into the state snapshots, so a
restart carries on from where it stopped.

## Example

The `feedback` example project: text drawn over a loop that zooms, turns, dims and shifts the hue
of the last frame a little, so the text leaves trails that drift outwards and change colour.
