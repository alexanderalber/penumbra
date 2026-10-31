# control-page

A web page that steers the project from a phone or a second browser. The page is generic: copy
`www/` into any project and it shows that project's controls.

The picture, `/show`, is an orange square on black (`/shape`, coloured by `/show`, a `tex.hsv`).
Four controls with `show` ticked drive it:

- `/size` (a slider, 0.1 to 1) sets the shape's size.
- `/look/hue` (a slider, -180 to 180, labelled `colour`) turns the colour round the colour
  wheel. It sits inside the component `/look`, so the page names it `look/colour`.
- `/round` (a toggle button) switches the square to a circle.
- `/flash` (a momentary button) turns the background white while it is held.

`/http` (a `chan.http_in` with its defaults) serves `www/index.html` at
`http://localhost:8090/`. Open that address in a browser while the example runs. The page draws
a slider per slider and a switch per toggle, labelled and with the slider's `min` and `max`.
Moving one changes `/show` at once. Two open pages follow each other.

**The momentary button:** the page shows `/flash` as a lamp that lights while it is held, on
the node or the show screen. The page cannot press it, because `chan.http_in` sets only the
`value` of a slider or a toggle.

**From a phone:** set `/http` `bind` to `lan`. Windows may ask whether penumbra may use the
network; allow it for private networks. `http.status` names the page's address under `page`,
for example `http://192.168.1.20:8090/`. Open it on a phone in the same network.

**Your own page:** copy `www/` into your project folder and add a `chan.http_in`. Tick `show` on
every `chan.slider` and `chan.button` the page should offer. The comment at the top of
`index.html` says what to change.

Try:

- `min` and `max` of `/size`: the page's slider follows.
- `show` off on `/round`: its switch leaves the page.
- Show mode (the padlock): the page keeps working, and the show screen shows the same controls.
