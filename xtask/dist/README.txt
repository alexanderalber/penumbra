penumbra {version} for Windows (64-bit)
=======================================

A node-based tool for projection mapping, camera blob tracking and audio- or DMX-driven
installations. Work in progress: things change between versions, including the project format.

Starting
--------

1. Unpack this folder anywhere (not inside the zip view of Explorer).
2. Double-click penumbra.exe. It opens a demo project.
3. More examples are in the examples folder, each showing one side of penumbra (feedback
   loops, sound, tracking, Python, 3D projection). Open one with the folder button at the
   left end of the status bar: Open project, then the example's folder. examples\README.md
   lists them.

Windows may warn "Windows protected your PC", because this program is not signed. Click
"More info", then "Run anyway".

What you need
-------------

- Windows 10 or 11, 64-bit, with a graphics card that supports DirectX 12 or Vulkan.
- Optional: Python 3.12 or later (python.org), only for script nodes. Everything else works
  without it.
- Optional: NDI Tools or the NDI Runtime (https://ndi.video/tools/), only for the NDI nodes
  (out.ndi, tex.ndi_in). NDI® is a registered trademark of Vizrt NDI AB.

Files
-----

- penumbra.exe              the app
- pyhost.dll                runs Python for script nodes; keep it next to penumbra.exe
- penumbra-cli.exe          command line tool (control channel client, soak test), for use
                            from a terminal; not needed to use the app
- docs\                     the documentation of this version (Markdown); start at
                            docs\README.md
- examples\                 example projects, listed in examples\README.md
- CLAUDE.md                 how Claude Code drives the app, read by it when started here
- THIRD-PARTY-NOTICES.html  licences of the components penumbra is built from
- LICENSE-MIT, LICENSE-APACHE

Driving penumbra from Claude Code or a script
---------------------------------------------

Everything the app does is a command on a local control channel: JSON lines over TCP on
127.0.0.1, port 9880. While penumbra.exe runs, from a terminal in this folder:

    penumbra-cli ctl ping
    penumbra-cli ctl commands
    penumbra-cli ctl node.list

Details in docs\control.md. To let Claude Code work with penumbra, start Claude Code in this
folder; it reads CLAUDE.md, which points it to the same documentation.

Source code: https://github.com/alexanderalber/penumbra

penumbra is licensed under the MIT license or the Apache License 2.0, at your option.
