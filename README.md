# penumbra

A node-based tool for projection mapping, camera blob tracking and audio- or DMX-driven
installations, written in Rust (egui + wgpu). Inspired by TouchDesigner, but deliberately not
compatible with it: no `.toe`/`.tox` files, no TouchDesigner Python API (script nodes are plain
Python), its own node families and names.

Built to be driven by Claude Code as much as by hand: every action is a command, reachable over
a local JSON control channel.

![penumbra: a node network that projects a title onto a wall, with the rendered room in the viewer](assets/screenshot.png)

**Status:** personal project, work in progress.

## Download (Windows)

No command line needed:

1. Download the zip from the [latest release](https://github.com/alexanderalber/penumbra/releases/latest)
   (under "Assets", `penumbra-...-windows-x64.zip`).
2. Unpack it anywhere and double-click `penumbra.exe`. It opens a demo project.

Windows may warn "Windows protected your PC", because the program is not signed: click
"More info", then "Run anyway". You need Windows 10 or 11 (64-bit) and a graphics card with
DirectX 12 or Vulkan. The zip also holds the [examples](examples/README.md) and the
documentation.

## Building from source

```
cargo run -p penumbra
```

With no arguments the app opens a demo project; more are in [examples](examples/README.md).
The [documentation](docs/README.md) starts with [getting started](docs/getting-started.md) and
has a reference of every [node](docs/reference/nodes.md) and [command](docs/reference/commands.md).

## Acknowledgements

penumbra owes a lot to the [storytold](https://github.com/storytold) apps (photocraft,
effectcraft, filmcraft and their siblings). Its former name, lumencraft, followed their "-craft"
naming, and many of its engineering conventions are borrowed from them: everything is a command,
a JSON control channel, headless snapshots, the no-panic rule, `cargo xtask ci` as the commit
gate. It also builds directly on several of their crates: `photocraft-codecs` and
`photocraft-psd` for image files and PSD layers, `effectcraft-text` for text, and the
`filmcraft-*` crates for video.
penumbra is an independent project and not affiliated with the storytold team.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state otherwise, any
contribution intentionally submitted for inclusion in penumbra, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or conditions.

## Disclaimer

penumbra is an independent project, not affiliated with, endorsed by or connected to
Derivative Inc. TouchDesigner is a trademark of Derivative Inc. Mentions of TouchDesigner in
this repository only describe where an idea came from.
