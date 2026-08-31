# auto-ascii

Realtime ASCII-art video for terminals — and a library you can draw the cells
with yourself.

An offline factory distills a reference video into a resolution-independent
feature asset (`.slpy`: luma, edge magnitude and orientation, highlights,
chroma — never glyphs). A runtime player maps that asset onto whatever cell
grid you have right now: glyph ramps, directional edge strokes, highlights and
half-blocks, letterboxed, reflowing live on resize, with temporal hysteresis so
nothing flickers. Because glyph choice happens at render time, one asset looks
right at 80x24 in a Linux console and at 320x90 in a GPU terminal.

Project: **https://github.com/andmckay01/auto-ascii**

## This npm package is a placeholder

**It contains no code.** Installing it gives you this README and the license,
nothing else — there is no entry point, no binary, and `require("auto-ascii")`
will not resolve.

The name is reserved here for a planned npm distribution of the player: either
prebuilt platform binaries selected at install time (the way `esbuild` ships
them) or a WebAssembly build of the render pipeline for use in Node and the
browser. Neither exists yet. When one does, it will ship under this name at
`0.1.0` or later, and this README will be replaced with real usage docs.

Version `0.0.x` means exactly that: not yet a working package.

## What works today

The implementation is Rust, in the repository linked above:

- **`auto-ascii`** — the library crate (the terminal `Player`, and a
  terminal-free `RenderSession` that hands back a grid of glyphs and RGB colors
  for your own renderer), plus the `sleepy-player` CLI binary.
- **`sleepy-factory`** — the offline factory that turns video into `.slpy`
  assets (uses ffmpeg as a subprocess).

Prebuilt Linux (glibc and static musl) and cross-built Windows player binaries
are produced by the repository's `scripts/release.sh`; macOS builds from
source. See the repository README for the quickstart, the palette table and the
embedding API.

## License

Dual-licensed under either of

- Apache License, Version 2.0
- MIT license

at your option. See the bundled `LICENSE` file, or `LICENSE-APACHE` and
`LICENSE-MIT` at the root of the repository.
