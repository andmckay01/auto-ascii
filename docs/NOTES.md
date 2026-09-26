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
no DIB-row or RIFF-chunk padding enters the pinned fixture bytes. The
frames are uncompressed so that no codec, colour conversion beyond a byte
permutation, or scaler sits in ffmpeg's decode path: every ffmpeg build
decodes them identically, so an ffmpeg upgrade cannot move the determinism
guard's pinned sha256 of the asset built from the fixture. See the
fixture contract in [INTERFACES.md](INTERFACES.md).

### Edge orientation and provenance

The factory follows a small Kang-style edge-tangent-flow variant: two
orientation-aware bilateral passes average doubled-angle vectors, while
local Scharr magnitude stays unsmoothed. Smoothing magnitude would broaden
thin contours before downsampling; preserving it and using hysteresis keeps
those contours available to the renderer. E is hysteresis-thresholded but
never thinned: non-maximum-suppression ridges are one pixel wide and break
up when the player box-resamples them into cells. The exact rational vector formula
and gradient-to-tangent convention are in [INTERFACES.md](INTERFACES.md).

### Color transforms and temporal smoothing

The RGB24 luma path linearizes sRGB, applies Rec.709 weights in Q16, then
looks up CIE L* in a 64 KiB table. [INTERFACES.md](INTERFACES.md) owns the
numeric LUT contract. Luma smoothing is kept light because stronger temporal
averaging ghosts motion. Edge magnitude and vectors (E/Ex/Ey) use heavier
averaging, since edge shimmer is the main flicker source; the player's
dual-threshold gate on E rides that decay. Chroma EMA
blends the full-precision channels after area averaging and before RGB565
packing; smoothing packed 5/6/5-bit channels would quantize twice.

### Letters highlight threshold

The letters codec uses plain picture tone for `LETTERS_FILL_MIN`, rather
than the ramp's contrast curve. Its value of 236 reserves solid fill for
true highlights (a lit window or a white core) and keeps it off ordinary lit
skin, where isolated blocks read as speckle rather than light.

### Shadow lift

The per-shot NORM window (p2→0, p98→255) is linear, so it cannot move a
dark subject relative to a bright one: on an 8–16 step glyph ramp, a
subject below the first step renders as the same glyph as black. Shadow
lift bends the 256-entry levels LUT toward the shadows to buy that subject
a step, at no per-pixel cost and on top of the per-shot normalization.
Black and white stay fixed, so it redistributes the middle rather than
washing the picture out; at full strength a mid-shadow 64 lands at 127.

## Technology

### Font measurement provenance

The conservative table measures printable ASCII as mean antialiased ink in a
64×128 cell using ffmpeg/libfreetype and DejaVu Sans Mono Book at 106 px.
DejaVu's advance is 1233/2048 em (about 64 px there), and its ascent plus
descent is (1901+483)/2048 em (about 123 px); the measured glyphs do not clip.
Coverage is `sum(gray)/(255·64·128)`. The table is a committed artifact;
`derive_coverage.py` reproduces it and needs ffmpeg plus the font, not a video
corpus.

The letters ramps were measured on Menlo and SF Mono as antialiased ink
over the advance-scaled cell, following the font-table measurement method;
each ramp is monotonic in both. Base glyphs were chosen for even top/bottom
mass so the ramp reads as tone rather than shape. The Menlo letters ramp
has a measured dense endpoint of 0.196.

The ASCII ramp's CoreText measurement uses the advance × (ascent + descent)
cell. It is strictly increasing on JetBrains Mono 2.304 Regular; Menlo
inverts the near-ties `+`/`r` and `#`/`D`, each within 0.003 coverage.
Its Q8 ink values normalize to `@` at 0.283 cell coverage.
See [fonts/README.md](../crates/auto-ascii-core/fonts/README.md) for font-table
generation and [INTERFACES.md](INTERFACES.md) for its design history.

### Unicode braille coordinates

For U+2800, dot-mask bits 0x01/0x02/0x04 map to left rows 1/2/3,
0x08/0x10/0x20 to right rows 1/2/3, and 0x40/0x80 to bottom left/right.
The compositor uses these masks for edges and texture, not solid fills;
its repertoire and density constraints remain pinned in tests.

### Terminal replies and palette stability

For [DEC synchronized output](https://gist.github.com/christianparpart/d8a62cc1ab659194337d73e399004036),
DECRPM answers 0 = not recognized, 1 = set, 2 = reset, 3 = permanently set
and 4 = permanently reset. Only 1 or 2 means the mode can be driven: the
spec's detection table marks 3 as undefined behaviour and 4 as recognized
but never honored, so only 1/2 enable frame wrapping. VTE answers
`CSI ? 2026 ; 4 $ y`, so wrapping frames in mode 2026 would waste bytes. The source anchors are VTE
`src/modes.py` (`CONTOUR_BATCHED_RENDERING`, `MODE_FIXED`, `ALWAYS_RESET`)
and `src/vteseq.cc` (`Terminal::DECRQM_DEC`). VTE answers CSI 14/18/19 t,
but not the CSI 16 t cell-size query; its `src/pty.cc` `Pty::set_size`
sets `ws_xpixel = ws_col * cell_width_px`, allowing the PTY fallback.
The terminal quirk table keys on queried identity, rather than `TERM`;
[INTERFACES.md](INTERFACES.md) owns the query and quirk contracts. In the
xterm-256 palette, slots 0–15 vary with the user's theme; the 6³ cube
(16–231) and grayscale ramp (232–255, values 8–238) are stable.

The historical Windows rationale for passive-only probing was that the
reply plumbing used POSIX termios/poll and conhost swallowed or mangled DCS
queries. Windows Terminal supported truecolor without exporting
`COLORTERM`, so `--tier truecolor` supplied the explicit override when
passive hints could not establish it. The passive-only contract remains in
[INTERFACES.md](INTERFACES.md).

### Unix signal-safe restoration

On Unix, the process-wide restore path writes the alt-screen-leave,
cursor-show and SGR-reset bytes and restores termios from SIGINT/SIGTERM,
panic, atexit or normal shutdown. The signal path uses atomics, raw `write(2)` and
`tcsetattr`, with no allocation or locks.

### Subprocess pipes and media geometry

The factory drains ffmpeg stderr concurrently with stdout so a full stderr
pipe cannot stall frame reads. Short raw RGB24 frames are errors. The corpus
preparer's `reverse`/`areverse` filters buffer whole streams in RAM, so its
boomerang option suits short clips. Its geometry runs in RGB24 to avoid
4:2:0 chroma shifts at odd crop offsets, then converts to yuv420p at output.
ffmpeg autorotates on decode, so the filtergraph sees post-rotation frames:
rotation side data swaps width and height, the sample aspect ratio rescales
width, and all layout math uses that post-rotation display size.

### Algorithm provenance

The factory's small incremental SHA-256 follows FIPS 180-4. It computes
the input-file and params-fingerprint hashes that name eval cache entries,
the determinism fingerprint and the import provenance hash. The player's
shadow lift blends toward integer `isqrt(n·255)`, not a `powf` gamma: float
results are not guaranteed bit-identical across platforms, and the render
goldens are byte-compared. Both terms are monotonic in `n`, so the ramp
never inverts, and 0 and 255 are fixed points.

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
open `.ascii` assets as read-only shared mappings (`memmap2::Mmap::map`,
`PROT_READ` + `MAP_SHARED`). A mapping's address does not change when its
`Mmap` handle moves, so slices into it outlive moves of the handle.
Read-only only stops this process writing through the mapping: another
process can still modify or truncate the file. Because the mapping is
shared, an in-place rewrite shows through slices the reader has already
validated, and touching a page past a truncation raises `SIGBUS`. Replacing
an asset by rename is safe, because the old inode stays mapped; rewriting
it in place is not.

### GIF frame timing

GIF frame delays count hundredths of a second. The review reel's delay is
floor(100/fps) centiseconds, minimum 1, so rates that do not divide 100
play fast (30 fps plays at about 33 fps, 24 fps at 25 fps) and every rate
above 50 fps plays at 100 fps.
