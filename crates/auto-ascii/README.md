# auto-ascii

Realtime ASCII-art video for terminals — and for whatever you want to draw it
with.

An offline factory distills reference video into a resolution-independent
feature asset (`.ascii`); this crate maps that asset onto whatever cell grid you
have right now — glyph ramps, directional edge strokes, highlights and
half-blocks, letterboxed, resize-correct, with temporal hysteresis so nothing
flickers. Assets never store glyphs: every glyph decision happens at render time
for *your* grid, *your* palette, *your* terminal.

```rust
auto_ascii::Player::builder().asset("intro.ascii").looping(true).build()?.run()?;
```

That is the whole player: capability probe, letterbox, live resize, terminal
restore on quit/Ctrl-C/panic. Own your event loop instead? `RenderSession` is
terminal-free — `render(frame, cols, rows)` hands back a `Grid<Cell>` of glyphs
and RGB colors for your renderer, game engine or test.

| feature | default | provides |
|---|---|---|
| `terminal` | **on** | `Player` / `PlayerBuilder` — the blocking terminal session (implies `compose`) |
| `audio` | **on** | plays an asset's soundtrack through the audio device (cpal) |
| `compose` | **on** | `Composition` TOML timelines (stitched clips) |
| `cli` | **on** | the `auto-ascii` command and its dependencies: clap, the factory (implies `terminal`) |
| *(none)* | | `RenderSession` only: no crossterm, no clap (`default-features = false`) |

Examples: `simple-play` (the whole player in one call), `embedded-loop`
(`RenderSession` in a hand-rolled loop with a mid-run resize), `headless-dump`
(frames to stdout as text, no terminal at all).

The crate also ships the `auto-ascii` command behind the default-on `cli`
feature, so `cargo install auto-ascii` installs it. Producing assets and
playing them from a shell (`auto-ascii add`, `auto-ascii play`) is its job —
the repository's README (`README.md` at the workspace root) covers that
workflow, and `docs/FEATURE-MAP.md` the glyph styles, the eight shipped
palettes and the eval harness. To embed the player without the command:

```toml
auto-ascii = { version = "0.3", default-features = false, features = ["terminal", "audio", "compose"] }
```

## License

MIT; the `LICENSE` file lives at the root of the repository.
