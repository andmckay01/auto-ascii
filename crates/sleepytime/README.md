# sleepytime

Realtime ASCII-art video for terminals — and for whatever you want to draw it
with.

An offline factory distills reference video into a resolution-independent
feature asset (`.slpy`); this crate maps that asset onto whatever cell grid you
have right now — glyph ramps, directional edge strokes, highlights and
half-blocks, letterboxed, resize-correct, with temporal hysteresis so nothing
flickers. Assets never store glyphs: every glyph decision happens at render time
for *your* grid, *your* palette, *your* terminal.

```rust
sleepytime::Player::builder().asset("intro.slpy").looping(true).build()?.run()?;
```

That is the whole player: capability probe, letterbox, live resize, terminal
restore on quit/Ctrl-C/panic. Own your event loop instead? `RenderSession` is
terminal-free — `render(frame, cols, rows)` hands back a `Grid<Cell>` of glyphs
and RGB colors for your renderer, game engine or test.

| feature | default | provides |
|---|---|---|
| `bin` | **on** | the `sleepy-player` CLI binary (implies `terminal`) |
| `terminal` | via `bin` | `Player` / `PlayerBuilder` — the blocking terminal session |
| *(none)* | | `RenderSession` only: no crossterm, no clap (`default-features = false`) |

Examples: `simple-play` (12 lines, the whole player), `embedded-loop`
(`RenderSession` in a hand-rolled loop with a mid-run resize), `headless-dump`
(frames to stdout as text, no terminal at all).

Producing assets is the separate `sleepy-factory` binary's job — the repository
README (`README.md` at the workspace root) covers the factory workflow, the
eight shipped palettes and the eval harness.

## License

Dual-licensed under Apache-2.0 or MIT, at your option.
