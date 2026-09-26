# Terminal checklist — the 5-minute manual pass

CI is headless: kitty, alacritty, wezterm, gnome-terminal and xterm
**cannot run there**, so nothing automated can claim "verified on kitty". What CI
*does* pin is each terminal's **identity**: the environment it exports, the
`TIOCGWINSZ` it sets and the exact bytes it answers our capability volley with
are replayed through the real probe on a real pty, and the resulting `Caps` are
asserted per terminal
(`crates/auto-ascii-term/tests/terminal_identity.rs`, seven fixtures; byte-level
parser coverage in `crates/auto-ascii-term/tests/probe_parser.rs`; the console render
floor in `crates/auto-ascii/tests/linux_console_golden.rs`).

What a machine cannot check is what this document is for: **fonts, real colors,
tearing, cell aspect, and whether it looks good.** Budget five minutes per
terminal.

---

## 0. Before you start

```bash
cargo build --release -p auto-ascii           # the player
ASSET=path/to/clip.ascii                      # any .ascii (auto-ascii import makes one)
PLAY="./target/release/auto-ascii-player $ASSET"
```

Keys during playback: `q`/`Esc` quit · `space` pause · `0`–`9` jump ·
`←`/`→` 5 s · `d` dial · `[` `]` adjust · `v` controls. The bottom-row progress
bar flashes for ~1 s after a seek, with the key-hints row just above it;
while paused it stays up and reads `PAUSED`. Resize the window at any time;
a resize (font zoom included) briefly raises the controls overlay, whose info
row ends in the grid size (`213x58 cells`). Below 160 columns the row above it
says Cmd - (Ctrl -) zooms out for a sharper picture, and from 240x36 on a
block tier the overlay text is drawn in big half-block letters.

**Read the probe's mind** on the terminal you are sitting in (prints one line
with the caps the shipping probe concluded — color tier, sync 2026, cell px,
glyph tier, and whether any reply bytes leaked):

```bash
cargo run -q -p auto-ascii-term --bin auto-ascii-term-harness -- caps
# PROBE-DONE ms=6 color=True sync=true can_query=true cellpx=10x20 support=UnicodeCore glyphs=7 stray=0
```

Raw replies, if you want to see the terminal talk (Ctrl-C to exit; the reply is
control bytes, hence `cat -v`):

```bash
printf '\033[>0q\033[?2026$p\033P+q524742\033\\\033[16t\033[c'; cat -v
```

---

## 1. The matrix

| Terminal | Expected caps | The one thing to look at |
|---|---|---|
| kitty | truecolor · sync 2026 · cell px from `CSI 16 t` | smooth 24-bit gradients, zero tearing |
| alacritty | truecolor · sync 2026 · cell px from winsize | same, and correct aspect without a `16 t` answer |
| wezterm | truecolor · sync 2026 · cell px from `CSI 16 t` | truecolor proven by the *query* path (no COLORTERM needed) |
| gnome-terminal (VTE) | truecolor · **no** sync 2026 · cell px from winsize | tearing on fast pans (expected — see quirks) |
| xterm (`xterm-256color`) | **256** · no sync · cell px from `CSI 16 t` | banding is correct here, not a bug |
| Linux console (`TERM=linux`) | 16 colors · CP437/ASCII ramp · no cell px | legibility at 80×24, no missing-glyph boxes |

Every row is asserted by a pty fixture; the columns below are the human half.

---

## 2. Per terminal

### kitty

```bash
$PLAY                      # then resize, and try: --palette unicode
```

*Expect:* full 24-bit color, half-block fills (`▀▄`) and box-drawing edge
strokes on the unicode palette, no tearing at all (kitty honors `?2026`), and
letterbox pads that stay exactly centered while you drag the window edge.

*Quirks worth knowing:*
- kitty answers our `XTGETTCAP RGB` query with `0+r` — it simply has no `RGB`
  entry in its capability tables (it advertises `Tc` instead). Truecolor there
  is concluded from `COLORTERM=truecolor`, which kitty exports — and since M5
  also from kitty's own XTVERSION reply: the identity-keyed quirk table
  (`auto-ascii-term/src/quirks.rs`, entry `kitty-rgbless-xtgettcap`) restores
  truecolor even when a launcher strips `COLORTERM`. `--no-quirks` shows the
  raw conclusion; `--tier truecolor` still forces everything.
- kitty's `CSI 16 t` answer wins over the kernel's winsize pixels, so font-size
  changes (`ctrl+shift+=`) re-derive the cell aspect on the next resize.

### alacritty

```bash
$PLAY
```

*Expect:* identical picture to kitty. The interesting part is invisible: it
answers only DA1 and DECRQM.

*Quirks:*
- No XTVERSION handler, no XTGETTCAP handler, and no `CSI 16 t` (it implements
  `14 t`/`18 t` only) — cell size comes from the kernel winsize, which
  alacritty fills in. If glyphs ever look vertically stretched, check
  `cellpx=` in the `caps` line above, then override with `--cell-aspect`.
- Synchronized output **is** supported (DECRQM answers `2`) since 0.13; an
  older alacritty answers `0` and you should expect mild tearing.

### wezterm

```bash
$PLAY
```

*Expect:* as kitty. wezterm is the only one of the five that proves truecolor
through the query path (`XTGETTCAP RGB` → `8/8/8`), so it should reach
truecolor even in a COLORTERM-stripped environment — worth a spot check:

```bash
env -u COLORTERM cargo run -q -p auto-ascii-term --bin auto-ascii-term-harness -- caps
# expect: color=True
```

### gnome-terminal (VTE)

```bash
$PLAY
```

*Expect:* truecolor and correct aspect, but **tearing on fast pans is normal
here** — VTE knows DEC mode 2026 and reports it *permanently reset*, i.e. it
will never honor a synchronized-update wrap, so we do not send one. `sync=false`
in the `caps` line is the correct reading, not a detection failure.

*Quirks:*
- No XTVERSION, no XTGETTCAP: everything comes from `COLORTERM`/`TERM` plus the
  winsize, which VTE fills with real cell metrics.
- Zooming (`ctrl+-`/`ctrl++`) changes the cell size; the next resize event
  re-reads it. If you zoom without resizing, aspect can lag by one frame.

### xterm

```bash
$PLAY                                   # TERM=xterm-256color
xterm -class UXTerm -u8 -fa Monospace -fs 11 -e "$PLAY"   # if your default xterm has a tiny font
```

*Expect:* **256-color** output — visible banding on smooth gradients is the
correct result, not a regression. xterm answers our `RGB` query with the value
`-1` ("no direct color") unless it is running in direct-color mode, and
plain xterm approximates `38;2` into its 256 palette anyway. Since M5 that
answer also *clamps* a stale `export COLORTERM=truecolor` from your shell
profile back to 256 (quirk `xterm-no-direct-color` — the queried terminal
beats the passive lie; `--no-quirks` to disable).

To see the truecolor path on xterm, start it in direct-color mode:

```bash
xterm -direct2 -e "$PLAY"   # TERM=xterm-direct*; caps line should read color=True
```

*Quirks:* xterm's DA1 digits vary with the `decTerminalID` resource — we use
DA1 only as the volley's end sentinel, so any form works. No mode 2026 (expect
tearing).

### Linux console (`TERM=linux`)

Switch to a VT (`ctrl+alt+F3`), log in, then:

```bash
$PLAY                       # 16 colors + the CP437-safe ASCII ramp are automatic
$PLAY --palette ascii       # same thing, forced
```

*Expect:* the picture in `" .:coO8@"` ink with 16 ANSI colors, letterboxed at
80×24, **no missing-glyph boxes and no mojibake** — the committed golden
(`crates/auto-ascii/tests/goldens/linux_console_80x24_f10.txt`) is what this
should look like in glyph terms. Sub-cell structure shows up as the `"` / `_`
subposition pair; nothing on this tier leaves printable ASCII, which is what
makes the "no boxes" expectation enforceable rather than hopeful (see
`every_glyph_is_console_printable` and auto-ascii-core's
`every_ascii_tier_glyph_is_ascii`). Cell aspect falls back to 2.0 (the console
reports no pixel geometry), which is correct for the standard 8×16 console
font.

*Quirks:* the console answers only DA1 (`ESC [ ? 6 c`); everything else times
out at 200 ms, which is why startup there feels a beat slower. `--no-query`
skips even that.

---

## 3. If something looks wrong

| Symptom | Try |
|---|---|
| Washed-out or banded color on a truecolor terminal | `--tier truecolor` (probe was too conservative — tell us the `caps` line) |
| Boxes / question marks instead of glyphs | `--palette ascii` (font lacks the block or box-drawing repertoire) |
| Picture too tall or too wide | `--cell-aspect 2.0` (or measure: `cellpx=WxH` → aspect = H/W) |
| Terminal hangs on start, or garbage keys | `--no-query` (never writes the volley) |
| A quirk-table correction looks wrong for your terminal | `--no-quirks` (re-runs the volley, bypassing the probe cache, and takes the replies at face value; the table is `auto-ascii-term/src/quirks.rs`, keyed on the XTVERSION reply) |
| Stale caps after changing terminal config | `--no-cache` (the probe cache lives at `$XDG_CACHE_HOME/auto-ascii/caps`) |

---

## 4. What the automated fixtures already cover

| Check | Where |
|---|---|
| Per-terminal `Caps` from canned reply streams (7 identities) | `crates/auto-ascii-term/tests/terminal_identity.rs` |
| Reply-byte parsing incl. the `RGB` *value* rule and DECRPM 3/4 | `crates/auto-ascii-term/tests/probe_parser.rs`, `src/probe.rs` unit tests |
| Probe never hangs, never leaks reply bytes into the app | `crates/auto-ascii-term/tests/pty_probe.rs` |
| Terminal always restored, backdrop reset included (drop / panic / SIGINT / SIGTERM / SIGHUP) | `crates/auto-ascii-term/tests/pty_restore.rs` |
| Per-tier escape streams (truecolor / 256 / 16 / mono) | `crates/auto-ascii-term/tests/tier_goldens.rs` |
| `TERM=linux` render golden + legibility floor | `crates/auto-ascii/tests/linux_console_golden.rs` |

Not covered by any of them, and hence this document: font coverage, actual
rendered color, perceived tearing, and taste.

---

## 5. Audit — no connectivity-specific code paths

Scope amendment (PLAN, top): connectivity engineering is out of scope. The
audit command and its current result:

```bash
rg -n -i -e 'SSH_CONNECTION|SSH_TTY|\bssh\b|conpty|tmux|telnet|downshift|governor|bandwidth' \
   --glob '!target/**' --glob '!docs/research/**' --glob '!docs/PLAN.md' --glob '!runs/**' \
   --glob '!Cargo.lock' .
```

**Result (M4): zero connectivity code paths.** The only hits are in
`crates/auto-ascii-term/src/probe.rs`, all of them the multiplexer flag in the
*cache key* — `(TERM, TERM_PROGRAM, COLORTERM, tmux?)` — which exists so an
inner session cannot inherit the outer terminal's proven capabilities. It
branches nothing: `probe.rs`'s
`multiplexer_flag_only_partitions_the_cache` test asserts the detected `Caps`
are byte-identical with the flag set and unset, and only the cache slot
differs. (The M4 pass also removed the last stale prose references to a
ConPTY backend and the descoped throughput governor from `auto-ascii-term`'s docs.)

Related, deliberately kept, and *not* connectivity code:
`SimBackend::set_throughput` — an in-memory writer that reports a simulated
`write_ns` so the eval harness can measure byte budgets. It never inspects the
environment and never changes what is rendered.
