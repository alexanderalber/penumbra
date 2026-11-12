penumbra {version} for macOS (Apple silicon)
============================================

A node-based tool for projection mapping, camera blob tracking and audio- or DMX-driven
installations. Work in progress: things change between versions, including the project format.

penumbra is developed and shown on Windows. The macOS build is new and less tested: expect
rough edges, and please report what you find.

Starting
--------

1. Unpack this folder anywhere.
2. This program is not signed, so macOS blocks it when it comes from a download. Allow it
   once, in Terminal:

       xattr -dr com.apple.quarantine "/path/to/this/folder"

3. Start it from Terminal, in this folder:

       ./penumbra

   It opens a demo project. A double-click on penumbra in Finder works too and opens a
   Terminal window beside the app; started that way it does not see the PATH of your shell,
   which matters for script nodes (see What you need).
4. More examples are in the examples folder, each showing one side of penumbra (feedback
   loops, sound, tracking, Python, 3D projection). Open one with the folder button at the
   left end of the status bar: Open project, then the example's folder. examples/README.md
   lists them.

macOS asks for permission the first time penumbra uses the camera, the microphone or the
screen. For a program started from Terminal the permission belongs to Terminal.

What you need
-------------

- A Mac with Apple silicon (M1 or later). There is no build for Intel Macs.
- Optional: Python 3.12 or later, only for script nodes. Everything else works without it.
  penumbra takes the first python3 on PATH that comes with its library (python.org's
  installer and Homebrew's python do), or the one named by the environment variable
  PENUMBRA_PYTHON (a libpython3.XY.dylib, or the folder holding one). Pictures and meshes
  in scripts also need NumPy in that Python.

Not on macOS: the Spout nodes (out.spout, tex.spout_in) share pictures between programs
on Windows only.

Files
-----

- penumbra                  the app
- libpyhost.dylib           runs Python for script nodes; keep it next to penumbra
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
