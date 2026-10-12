penumbra {version} for Windows (64-bit)
=======================================

A node-based tool for projection mapping, camera blob tracking and audio- or DMX-driven
installations. Work in progress: things change between versions, including the project format.

Starting
--------

1. Unpack this folder anywhere (not inside the zip view of Explorer).
2. Double-click penumbra.exe. It opens a demo project.

Windows may warn "Windows protected your PC", because this program is not signed. Click
"More info", then "Run anyway".

What you need
-------------

- Windows 10 or 11, 64-bit, with a graphics card that supports DirectX 12 or Vulkan.
- Optional: Python 3.12 or later (python.org), only for script nodes. Everything else works
  without it.

Files
-----

- penumbra.exe              the app
- pyhost.dll                runs Python for script nodes; keep it next to penumbra.exe
- penumbra-cli.exe          command line tool (control channel client, soak test), for use
                            from a terminal; not needed to use the app
- THIRD-PARTY-NOTICES.html  licences of the components penumbra is built from
- LICENSE-MIT, LICENSE-APACHE

Documentation and source code: https://github.com/alexanderalber/penumbra

penumbra is licensed under the MIT license or the Apache License 2.0, at your option.
