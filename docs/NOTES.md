# Notes

## Domain

The existing format and rendering contracts remain in
[INTERFACES.md](INTERFACES.md) and [FEATURE-MAP.md](FEATURE-MAP.md).
### Deterministic metadata evolution

The META chunk is a CBOR map. Its factory provenance records the input file
name, never an absolute path, host, user, or wall-clock time. `ciborium`
serializes struct fields in declaration order; new fields are append-only
and use `serde(default)` so older readers ignore unknown keys and newer
readers accept older assets. See [FEATURE-MAP.md](FEATURE-MAP.md) for the
container's deterministic byte contract.

### AVI fixture byte layout

The evaluation fixture stores uncompressed `BI_RGB` frames in RIFF AVI.
DIB rows run bottom-up and pixels run B, G, R. Its width is a multiple of
four, making each row four-byte aligned and each frame chunk even-sized;
no DIB-row or RIFF-chunk padding enters the pinned fixture bytes. See the
fixture contract in [INTERFACES.md](INTERFACES.md).

### Edge orientation and provenance

The factory follows a small Kang-style edge-tangent-flow variant: two
orientation-aware bilateral passes average doubled-angle vectors, while
local Scharr magnitude stays unsmoothed. Smoothing magnitude would broaden
thin contours before downsampling; preserving it and using hysteresis keeps
those contours available to the renderer. The exact rational vector formula
and gradient-to-tangent convention are in [INTERFACES.md](INTERFACES.md).

### Color transforms and temporal smoothing

The RGB24 luma path linearizes sRGB, applies Rec.709 weights in Q16, then
looks up CIE L* in a 64 KiB table. [INTERFACES.md](INTERFACES.md) owns the
numeric LUT contract. Luma smoothing is kept light because stronger temporal
averaging ghosts motion; edge vectors use more averaging to reduce shimmer,
with the player's dual-threshold gate following their decay.

## Technology

### Font measurement provenance

The conservative table measures printable ASCII as mean antialiased ink in a
64×128 cell using ffmpeg/libfreetype and DejaVu Sans Mono Book at 106 px.
DejaVu's advance is 1233/2048 em (about 64 px there), and its ascent plus
descent is (1901+483)/2048 em (about 123 px); the measured glyphs do not clip.
Coverage is `sum(gray)/(255·64·128)`. The table is a committed artifact;
`derive_coverage.py` reproduces it and needs ffmpeg plus the font, not a video
corpus. The Menlo letters ramp has a measured dense endpoint of 0.196; the
JetBrains Mono ASCII ramp normalizes Q8 ink to `@` at 0.283 cell coverage.
See [fonts/README.md](../crates/auto-ascii-core/fonts/README.md) for font-table
generation and [INTERFACES.md](INTERFACES.md) for its design history.

### Unicode braille coordinates

For U+2800, dot-mask bits 01/02/04 map to left rows 1/2/3,
08/10/20 to right rows 1/2/3, and 40/80 to bottom left/right.
The compositor uses these masks for edges and texture, not solid fills;
its repertoire and density constraints remain pinned in tests.

### Terminal replies and palette stability

[DEC synchronized output](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036)
uses DECRPM 0 for unrecognized, 1/2 for set/reset and driveable, and 3/4
for permanently set/reset. VTE answers `CSI ? 2026 ; 4 $ y`, so wrapping
frames in mode 2026 would waste bytes. The source anchors are VTE
`src/modes.py` (`CONTOUR_BATCHED_RENDERING`, `MODE_FIXED`, `ALWAYS_RESET`)
and `src/vteseq.cc` (`Terminal::DECRQM_DEC`). VTE answers CSI 14/18/19 t,
but not the CSI 16 t cell-size query; its `src/pty.cc` `Pty::set_size`
sets `ws_xpixel = ws_col * cell_width_px`, allowing the PTY fallback.
The terminal quirk table keys on queried identity, rather than `TERM`;
[INTERFACES.md](INTERFACES.md) owns the query and quirk contracts. In the
xterm-256 palette, slots 0–15 vary with the user's theme; the 6³ cube
(16–231) and grayscale ramp (232–255, values 8–238) are stable.

### Signal-safe restoration

The process-wide restore path writes the alt-screen-leave, cursor-show and
SGR-reset bytes and restores termios from SIGINT/SIGTERM, panic, atexit or
normal shutdown. The signal path uses atomics, raw `write(2)` and
`tcsetattr`, with no allocation or locks.

### Subprocess pipes and media geometry

The factory drains ffmpeg stderr concurrently with stdout so a full stderr
pipe cannot stall frame reads. Short raw RGB24 frames are errors. The corpus
preparer's `reverse`/`areverse` filters buffer whole streams in RAM, so its
boomerang option suits short clips. Its geometry runs in RGB24 to avoid
4:2:0 chroma shifts at odd crop offsets, then converts to yuv420p at output.
ffmpeg autorotates on decode; sample-aspect-ratio and rotation side data
therefore have to be applied before computing displayed canvas dimensions.

### Algorithm provenance

The factory's small incremental SHA-256 follows FIPS 180-4; it hashes the
eval cache key, determinism fingerprint and import provenance. The player's
integer shadow lift uses `isqrt(n·255)` for a portable monotonic tone curve
with fixed black and white endpoints. GIF delays count centiseconds, so the
review reel cannot represent intervals below 10 ms.

### Overlay size rationale

At 240 columns, one-cell text is one-third of its apparent 80-column size;
four-cell-wide large glyphs restore the apparent width and leave a
60-character row. A 36-row floor lets three 3-row overlay bands occupy at
most one quarter of the screen. See [FEATURE-MAP.md](FEATURE-MAP.md) for
rendered overlay behavior.


### Comment extraction

The checker ports the step-1 inventory's language handling and fourteen
fixture cases. Rust uses
[rust-analyzer's rustc lexer distribution](https://docs.rs/ra-ap-rustc_lexer/0.174.0/ra_ap_rustc_lexer/),
which distinguishes comments from raw, byte and C strings, characters and
lifetimes, and handles nested block comments. A token pass recognizes
literal documentation attributes, including nested `cfg_attr` and macro
definitions. It does not expand declarative or procedural macros: attributes
assembled entirely by expansion are outside this source check.

[Tree-sitter Bash](https://github.com/tree-sitter/tree-sitter-bash) and
[Tree-sitter TOML](https://github.com/tree-sitter-grammars/tree-sitter-toml)
provide comment ranges without interpreting literal content as comments.
Parse errors fail closed. Python uses its standard `tokenize` and `ast`
modules; tokenizer columns count Unicode characters while AST columns count
UTF-8 bytes. Python 3.12+ exposes comments inside f-string expressions to
the tokenizer. The helper runs in an isolated interpreter, reads source on
stdin and executes none of the scanned code.

Make recipes use the shell grammar after masking recipe prefixes, Make
variable references and doubled dollars without moving byte offsets.
Continued recipes preserve quote state; `.ONESHELL` preserves context over
adjacent recipe lines. Literal `.RECIPEPREFIX` values and inline target
recipes are covered. A dynamic recipe prefix fails extraction. Make
assignment comments honor escaped hashes and continuations; quotes outside
recipes do not protect hashes. Hashes within Make variable/function references
and `define` bodies are literal data, as specified by the
[GNU Make manual](https://www.gnu.org/software/make/manual/html_node/Makefile-Contents.html).
Like heredocs and generated-text literals, that data is
not recursively interpreted, nor are Make variable/eval expansions run.

Adjacent standalone line comments of the same kind form one cluster;
trailing comments stay separate. Counts use inclusive physical line spans,
including blank documentation lines, matching the audit. Policy checks
count only the header's text lines. These are different measurements.

### PTY soak harness

`tools/soak.py` starts the player with `pty.fork()`, which makes the new
pty the child's controlling terminal. Only then does `TIOCSWINSZ` on the
master deliver a real `SIGWINCH`, so each storm resize reaches the player
exactly as a user resizing a terminal window would. RSS samples come from
`VmRSS` in `/proc/<pid>/status`, which exists on Linux only; on macOS the
harness still storms and validates escape streams but `rss.csv` holds only
its header. The RSS slope (< 1 MB/h after warmup, PLAN M5) is reported for a
reviewer, not gated by the harness exit code.

### Memory-mapped assets

The player, `RenderSession`, compositions, the CLI and the factory's eval
open `.ascii` assets as read-only private `memmap2` mappings. A mapping's
address does not change when its `Mmap` handle moves, so slices into it
outlive moves of the handle. Read-only and private only stop this process
writing through the mapping: another process can still modify or truncate
the file, and touching a page past a truncation raises `SIGBUS`. Replacing
an asset by rename is safe, because the old inode stays mapped; rewriting
it in place is not.

### GIF frame timing

GIF frame delays count hundredths of a second, so the review reel's delay
is a whole number of centiseconds and never below 10 ms: rates above
100 fps play at 100 fps.
