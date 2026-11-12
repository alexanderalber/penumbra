penumbra {version} for Linux (64-bit x86)
=========================================

A node-based tool for projection mapping, camera blob tracking and audio- or DMX-driven
installations. Work in progress: things change between versions, including the project format.

penumbra is developed and shown on Windows. The Linux build is new and a preview: less
tested, and without the parts listed under What you need. Please report what you find.

Starting
--------

1. Unpack this folder anywhere.
2. Start it from a terminal, in this folder:

       ./penumbra

   It opens a demo project.
3. More examples are in the examples folder, each showing one side of penumbra (feedback
   loops, sound, tracking, Python, 3D projection). Open one with the folder button at the
   left end of the status bar: Open project, then the example's folder. examples/README.md
   lists them.

What you need
-------------

- A 64-bit x86 Linux as new as Ubuntu 24.04 (the build is made there and needs its C
  library, glibc 2.39, or a newer one), with X11 or Wayland.
- A graphics card with a Vulkan driver.
- The system libraries for sound and devices, which a desktop installation has: ALSA
  (libasound2), udev (libudev1) and xkbcommon (libxkbcommon0).
- Optional: Python 3.12 or later with its shared library, only for script nodes.
  Everything else works without it. penumbra takes the newest libpython3.XY.so.1.0 in the
  system's library folders (on Ubuntu: sudo apt install libpython3.12), or the one named by
  the environment variable PENUMBRA_PYTHON (the library, or the folder holding it). Pictures
  and meshes in scripts also need NumPy in that Python (sudo apt install python3-numpy).

Not in this build yet, because they work on Windows only so far: cameras (tex.video_in,
and with it scanning and tracking from a live camera), screen capture (tex.screen_in),
Spout (out.spout, tex.spout_in), the SpaceMouse, arranging the displays and the
show-machine checks, and keeping the displays awake during a show.

Files
-----

- penumbra                  the app
- libpyhost.so              runs Python for script nodes; keep it next to penumbra
- penumbra-cli              command line tool (control channel client, soak test), for use
                            from a terminal; not needed to use the app
- docs/                     the documentation of this version: docs/penumbra.html in a
                            browser (all of it in one page; the ? on a node opens it
                            there), or the Markdown files, starting at docs/README.md.
                            It is written for Windows where it names keys and paths.
- examples/                 example projects, listed in examples/README.md
- library/                  the content library: effects for scanned objects, offered in
                            the add dialog's comp tab; library/README.md shows each
- CLAUDE.md                 how Claude Code drives the app, read by it when started here
- THIRD-PARTY-NOTICES.html  licences of the components penumbra is built from
- LICENSE-MIT, LICENSE-APACHE

Driving penumbra from Claude Code or a script
---------------------------------------------

Everything the app does is a command on a local control channel: JSON lines over TCP on
127.0.0.1, port 9880. While penumbra runs, from a terminal in this folder:

    ./penumbra-cli ctl ping
    ./penumbra-cli ctl commands
    ./penumbra-cli ctl node.list

Details in docs/control.md. To let Claude Code work with penumbra, start Claude Code in this
folder; it reads CLAUDE.md, which points it to the same documentation.

Source code: https://github.com/alexanderalber/penumbra

penumbra is licensed under the MIT license or the Apache License 2.0, at your option.
