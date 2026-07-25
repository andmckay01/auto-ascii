<!-- Terminal backends & capabilities — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../../PLAN.md -->

# TERMINAL BACKENDS & CAPABILITIES — digest

## Capability detection

**Passive (env/terminfo) — fast, often lies:**
- `COLORTERM=truecolor|24bit` → truecolor. Caveat: SSH does not forward COLORTERM by default (not in AcceptEnv); tmux may strip it.
- `TERM`: `*-256color` → 256; `xterm-kitty`, `wezterm`, `alacritty`, `xterm-ghostty`, `foot` → GPU tier; `linux` → console tier (16 color, limited glyphs); `screen*`/`tmux*` → multiplexer, inner caps ≠ outer caps.
- `TERM_PROGRAM` (iTerm.app, WezTerm, vscode, Apple_Terminal) + `TERM_PROGRAM_VERSION`.
- terminfo: `colors#`, `RGB`/`Tc` extended flags (need extended-cap lookup; ncurses `tigetflag` misses user caps unless using `-x`). Treat terminfo as fallback, not truth.

**Active queries — truthful, need timeout discipline:**
- XTVERSION `CSI > 0 q` → `DCS > | name version ST`. Answered by kitty, ghostty, wezterm, foot, contour, iTerm2, xterm ≥354. Best single terminal-identifier.
- DA2 `CSI > c` → type;version (xterm patch level; VTE encodes version; useful when XTVERSION silent).
- DA1 `CSI c` → universally answered by anything interactive. **Use as sentinel: send it LAST in the query volley; when its reply arrives, all earlier queries that were going to answer have answered.**
- XTGETTCAP `DCS + q <hex> ST` for `RGB`/`Tc` (kitty/ghostty/wezterm/foot/contour/xterm). Alt truecolor probe: `CSI 38:2::1:2:3 m` then DECRQSS `DCS $ q m ST` and parse echoed SGR.
- DECRQM `CSI ? 2026 $ p` → `CSI ? 2026 ; Ps $ y`; Ps=1|2 = sync supported, 0 = unrecognized. **Never hardcode a 2026 support table; query.**
- Geometry: `CSI 18 t` → cells (`CSI 8;rows;cols t`); `CSI 14 t` → text-area px (`CSI 4;h;w t`); `CSI 16 t` → cell px (`CSI 6;h;w t`). Prefer `ioctl(TIOCGWINSZ)` for cells (always works); use 16t only to get true cell aspect ratio for the 16:9 letterbox math (fallback assumption: cell w:h = 1:2). tmux often returns 0 for pixel queries.
- **Timeout recipe:** raw mode on; write whole volley (XTVERSION, DECRQM 2026, XTGETTCAP RGB, CSI 16t, then DA1) in one write; `poll()` stdin, accumulate replies until DA1 reply parsed OR deadline (150–250 ms local, ~1 s if `SSH_CONNECTION` set). If nothing arrives (piped output, Linux console quirks, `!isatty`) → assume dumb tier. Cache result keyed on (TERM, TERM_PROGRAM, ssh?, tmux?) to skip volley next launch.

**Tier classification:** (A) known GPU terminal name via XTVERSION → fast tier; (B) truecolor proven or COLORTERM → truecolor; (C) TERM 256color → 256; (D) terminfo colors=8/16 or TERM=linux → 16/mono + restricted glyphs. Separately flag `throughput: slow` if SSH_CONNECTION or tmux or ConPTY detected.

## Output path

- **The bottleneck is escape bytes through the PTY + the terminal's parser, not your renderer.** Naive truecolor: `ESC[38;2;R;G;Bm ESC[48;2;R;G;Bm X` ≈ 40 B/cell → 100×56 grid ≈ 220 KB/frame ≈ 6.6 MB/s at 30 fps. Trivial for kitty/foot/alacritty/wezterm (parsers do >100 MB/s); fatal over SSH (bandwidth + latency) and painful on old VTE.
- Mitigations by payoff:
  1. **SGR run-length elision:** emit color codes only when fg/bg differ from previous cell. Video is spatially coherent — often 5–20× byte reduction. On slow tiers, quantize colors harder to lengthen runs.
  2. **Diff/damage rendering:** keep previous frame's Cell grid; per row, emit changed spans; between spans, "skip vs move" heuristic — rewrite ≤ ~6 unchanged cells rather than emit a CUP (`CSI r;cH` costs ~8 B).
  3. **Full repaint is fine on GPU terminals on a local PTY** — simpler, immune to desync — keep it as the fast-tier default; switch to diff when SSH/tmux/slow detected or when measured frame bytes exceed budget.
  4. **Sync output (DEC 2026):** wrap frame in `CSI ?2026h … CSI ?2026l` → atomic present, no tearing. Supported: kitty, foot, WezTerm, Ghostty, Contour, iTerm2 3.5+, Alacritty 0.13+, recent VTE/GNOME Terminal, Windows Terminal 1.24+, tmux (buffers until `l` or ~1 s timeout). Harmless if unsupported — always emit when DECRQM says yes.
  5. **One `write(2)` per frame** from a preassembled buffer (≥64 KB, reuse allocation). Never per-cell writes, never stdio auto-flush.
  6. Home with `CSI H` and overwrite; never `ED` (clear) per frame; erase row tails with `CSI K`.
- **Session setup:** alt screen `CSI ?1049h`, hide cursor `CSI ?25l`, disable autowrap `CSI ?7l` (or never write the bottom-right cell), termios raw (-ICANON -ECHO, VMIN=0). Restore everything (+ `SGR 0`, show cursor, main screen, cooked mode) via atexit + SIGINT/SIGTERM handler + (Rust) Drop — a hosed terminal is the #1 TUI bug report.
- **Pacing:** latest-frame-wins. If the PTY write would block (slow SSH), drop frames — never queue. Measure per-frame write time; if sustained > frame budget, auto-downshift tier (fewer colors, diff mode, lower effective grid).

## Resize

- SIGWINCH handler sets an atomic flag / writes a self-pipe (eventfd) that the render loop polls alongside stdin. On wake: `TIOCGWINSZ`, re-query `CSI 16t` (font zoom changes cell aspect), recompute letterbox, force full repaint.
- Debounce trailing-edge 50–100 ms (drag = event storm); during storm just clear once and wait.
- tmux: pane size = min over attached clients; pixel queries usually 0 → assume 1:2 cells; don't rely on `allow-passthrough`.
- Windows/ConPTY: no SIGWINCH — consume `WINDOW_BUFFER_SIZE_EVENT` from `ReadConsoleInput`; set `ENABLE_VIRTUAL_TERMINAL_PROCESSING`; ConPTY re-translates VT (latency, historically scrubs sequences) → classify as slow tier. 80/20: make Windows work, don't optimize it.

## Tiers worth shipping

- **Color:** truecolor (`38;2`) → 256 (`38;5`; 6×6×6 cube + 24 grays, precomputed RGB→index LUT) → mono (glyph ramp carries everything). 16-color buys little over mono-with-one-accent; implement last or skip.
- **Glyphs:**
  - Pure ASCII 0x20–7E: universal floor; density ramps like ` .:-=+*#%@`.
  - **Shades ░▒▓█ (U+2591–2593, 2588) + half-block ▀▄ (2580/2584): the single biggest visual win** — half-block with distinct fg/bg = 2 vertical pixels per cell, doubling vertical resolution AND correcting the 1:2 cell aspect. Cheap, near-universal font support.
  - Box/diagonal drawing: useful only for the edge/contour layer; ASCII `/\|_-` approximations acceptable at floor tier.
  - Quadrants (2596–259F): 2×2 subcells but still one fg+bg pair — marginal over half-blocks; skip v1.
  - Braille (2800–28FF): 2×4 dots = high mono resolution, but 1-bit, fg-only, font rendering inconsistent (dot spacing/blank-cell width) — poor fit for layered luminance video. Skip under 80/20; optional novelty palette later.
- Ship 3 repertoires (ASCII / +shades+halfblock / full blocks) × 3 color modes ≈ the required 3–10 palettes.

## Lessons (notcurses, chafa, timg)

- **notcurses:** copy the DA1-sentinel query volley, per-terminal quirk table keyed on *queried identity* not TERM, and damage-map diff with SGR elision — it proves 30+ fps fullscreen cell video is realistic. Avoid copying its scope (planes/widgets/sixel/kitty-graphics — irrelevant, we're glyph-only).
- **chafa:** best cell→glyph selection: per-glyph 8×8 coverage bitmaps, pick glyph+fg/bg minimizing error via popcount; its cost is why it's offline-speed. Copy the coverage-bitmap idea but hoist the expensive matching into the factory (bake per-cell layer features; runtime does cheap ramp lookup). Copy its symbol-class taxonomy (ascii/block/border/braille) as palette definitions.
- **timg:** copy truecolor-half-block-by-default and drop-frames-to-stay-realtime pacing.

## Recommended backend abstraction

```rust
trait Backend {
    fn init(opts: Opts) -> Result<Self>;          // raw mode, alt screen, hide cursor, query volley (or cached caps)
    fn caps(&self) -> Caps;                       // see below; immutable after init except cell_px on resize
    fn events(&self) -> Receiver<Event>;          // Event::{Resize(cols,rows), Key(Key), Quit}; wraps SIGWINCH self-pipe + stdin
    fn begin_frame(&mut self);                    // CSI ?2026h if caps.sync, reset diff cursor state
    fn blit(&mut self, grid: &FrameGrid);         // backend does diff, SGR elision, cursor moves, buffer assembly
    fn end_frame(&mut self) -> FrameStats;        // CSI ?2026l, single write+flush; {bytes, cells_changed, write_ns} → feeds perf budgets/auto-tiering
    fn resize(&mut self, cols: u16, rows: u16);   // realloc diff buffers, force full repaint next frame
    fn shutdown(&mut self);                       // restore terminal; must also run from Drop/signal path
}

struct Caps {
    color: ColorTier,                 // True | C256 | C16 | Mono
    glyphs: GlyphFlags,               // ASCII | SHADES | HALFBLOCK | QUADRANT | BOX | BRAILLE
    sync_2026: bool,
    cells: (u16, u16),
    cell_px: Option<(u16, u16)>,      // for aspect math; None → assume 1:2
    throughput: Throughput,           // Fast | Normal | Slow (ssh/tmux/conpty)
    can_query: bool,
}

struct Cell { ch: char, fg: Rgb, bg: Rgb, attrs: Attrs /* BOLD|DIM|REVERSE */ }
```

Division of labor: the engine always emits final glyphs (palette chosen using `caps.glyphs` — realtime glyph selection stays engine-side per requirement 4) and truecolor RGB; the backend quantizes color to its tier, diffs, and moves bytes. Backend never substitutes glyphs. `FrameStats` is the hook for requirement 9's perf budgets and automated tier regression tests.

Sources: [contour vt-extensions: synchronized-output spec](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md), [tmux PR #4744 (DECSET 2026)](https://github.com/tmux/tmux/pull/4744), [bubbletea #850 (2026 support map)](https://github.com/charmbracelet/bubbletea/issues/850)
