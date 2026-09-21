<!-- Prior art & stack choice — produced by the ascii-engine-plan workflow, 2026-07-24; input to ../PLAN.md -->

PRIOR ART & STACK CHOICE — DIGEST

## 1. Prior-art survey (license / reuse verdict)

**Renderers**
- **chafa** (LGPL-3.0+, C): Best-in-class glyph selection — matches image cells against pre-analyzed glyph coverage bitmaps ("work cells"), picks symbol+fg/bg colors minimizing error; auto-probes terminal capabilities; sixel/kitty/iTerm2 protocols. *Reuse: the concept only* — per-glyph 8x8 coverage bitmap matching is exactly requirement #4's realtime character selection. LGPL blocks static linking into an MIT/Apache binary; shelling out is possible but wrong for a realtime engine. Steal the algorithm, not the code.
- **libcaca** (WTFPL, C): Ancient but proved that dithering (Floyd-Steinberg over 16 colors) beats naive quantization on low-color terminals. *Reuse: dithering-per-capability-tier idea for our lowest tier.* License is fine but code is legacy C, image-oriented; don't depend.
- **notcurses** (Apache-2.0, C): The strongest prior art. Its **blitter ladder** — space→half-blocks(1×2)→quadrants(2×2)→sextants(3×2)→octants→braille(4×2) — is a ready-made capability-degradation model (requirement #2/#7); its **plane stack** (z-ordered composable layers) prefigures requirement #5; `ncplayer` already does terminal video via ffmpeg. *Reuse: the tier taxonomy and terminfo-driven capability detection logic.* Rust bindings (`libnotcurses-sys`) exist but are thinly maintained and drag a C build dep — don't bind; reimplement the ~small subset we need.
- **timg** (GPL-2.0, C++): Video via libav, quarter/half blocks + kitty/iTerm2/sixel graphics. *Lesson:* linking libav directly caused it recurring version-churn pain; also proof that per-frame full redraw at modest grids hits realtime easily. GPL — no code reuse.
- **hiptext** (Apache-2.0, C++, jart): Renders glyphs with FreeType and matches against font raster — font-aware selection, highest fidelity approach. *Lesson (negative):* infamous build (ragel, ffmpeg, freetype, glog...), effectively unbuildable today — cautionary tale for our dependency policy. License compatible but code is dead.
- **mpv `--vo=tct`** (GPL/LGPL): ~200 lines: swscale to grid, half-block truecolor, full-frame escape-sequence writes, no diffing. *Lesson:* on fast terminals, brute-force full-frame writes with truecolor half-blocks sustain 30fps fine; throughput ceiling is escape-sequence bytes/frame — minimize SGR changes by run-length grouping same-color cells.
- **video-to-ascii projects** (joelibaceta/video-to-ascii MIT-Python; ASCII-generator GPL; jp2a GPL-2): All do naive per-pixel→ramp lookup at decode time, single luminance ramp, resolution decided per run. *Lesson:* pure-Python per-cell loops don't hold 24fps at large grids without numpy vectorization; none separate offline analysis from render.
- **ascii-live** (GPL-3.0, Go): Streams precomputed fixed-size ASCII frames over HTTP/curl. *Lesson:* it's the exact anti-pattern requirement #6 forbids — resolution baked into asset, breaks on resize. Useful as a distribution idea (curl-able demos) only.

**TUI infra**
- **crossterm** (MIT, Rust): raw mode, alt-screen, resize events, Windows support. *Use it* — but only for setup/events; render by writing pre-built escape-sequence byte buffers to a `BufWriter<Stdout>` in one syscall per frame, not via per-cell crossterm commands (per-command overhead kills fps).
- **termion** (MIT, Rust): Unix-only, less maintained. Skip.
- **ratatui** (MIT, Rust): Double-buffer cell diffing → minimal cursor moves. Diffing is our friend on *slow* terminals (tmux/SSH tier: only redraw changed cells), marginal on video-dense frames. *Verdict:* don't build the video path on ratatui's widget model; optionally embed for menus/HUD. Cheaper to implement own diff over our own cell grid (~150 LOC).
- **bubbletea** (MIT, Go): Elm architecture, full-frame string renderer. *Lesson:* framerate-based render loop decoupled from input events is the right structure. Not a dep (Go).
- **textual/rich** (MIT, Python): Segment caching makes Python TUIs feel fast, but full-screen 30fps video at 200×50+ cells is beyond comfortable Python ceiling. Confirms: player should not be Python.

## 2. Offline pipeline tooling

- **ffmpeg CLI subprocess** ✅ RECOMMENDED. `ffmpeg -i in.mp4 -vf scale=W:H,fps=24 -f rawvideo -pix_fmt rgb24 -` piped to stdin of the factory. Zero build pain, works on every OS, trivially parallel per-shot, `ffprobe -print_format json` for metadata. Even has built-in `edgedetect` (Canny) and `sobel` filters if we want a zero-code baseline.
- **ffmpeg-next (libav Rust bindings)** ❌: requires system ffmpeg dev headers + bindgen/clang; breaks across ffmpeg 5/6/7 API churn; Windows CI misery. Not worth it for an *offline* tool where subprocess latency is irrelevant.
- **opencv-rust** ❌: worst build pain in the Rust ecosystem (system OpenCV + clang, multi-minute builds, version pinning hell). Avoid.
- **OpenCV-Python** (viable fallback): `opencv-python-headless` wheels just work; Canny/Sobel one-liners; fast iteration for metric experiments. Cost: second toolchain, venv distribution pain, asset-format code duplicated across languages.
- **Pure-Rust CV**: `image` + `imageproc` (both MIT) provide Sobel gradients, Canny, morphology, integral images — fully sufficient for our edge/contour/highlight extraction. Offline speed via `rayon`. This closes the gap that usually forces Python.

## 3. Stack recommendation

| Option | Perf | Dep pain | Dev speed | Distribution | 80/20 fit |
|---|---|---|---|---|---|
| All-Rust (ffmpeg subprocess) | ★★★ | low | good | single static binary ×2 | **best** |
| Rust player + Python factory | ★★★ player | med (two toolchains) | best for factory experiments | binary + venv | ok |
| All-Python(+numpy) | player fails worst-case tiers | low | best | poor | fails req 1/2 |
| Go | ★★☆ (GC pauses fine, but weak image/CV ecosystem) | low | good | single binary | weaker libs |

**ONE recommendation: all-Rust, two binaries in one Cargo workspace (`sleepy-player`, `sleepy-factory`), video decode via ffmpeg CLI subprocess.** Decisive reasons: (a) asset format defined once as shared serde structs in a common crate — the factory⇄player contract is the riskiest interface and a two-language split doubles it; (b) `cargo test` + `insta` gives requirement #9's golden-frame/fuzz loop in one harness; (c) single-binary distribution for both; (d) imageproc removes the only real reason to want Python.

**Starter dependency list (crates):**
- *shared crate:* `serde`, `bincode` (or `rmp-serde`), `zstd`, `anyhow`, `thiserror`
- *player:* `crossterm`, `memmap2` (stream asset), `clap`, `unicode-width`; optional: `ratatui` (HUD only)
- *factory:* `image`, `imageproc`, `rayon`, `ndarray` (field resampling), `clap`, `indicatif`, `serde_json` (ffprobe parsing); ffmpeg via `std::process::Command`
- *test/bench:* `insta` (golden frames), `proptest` (resize fuzzing), `criterion` (perf budgets)

**Asset format sketch (details owned by format agent):** custom chunked container = header (bincode) + per-frame zstd-compressed planar fields (u8-quantized luminance, edge magnitude+orientation, highlight mask) at one fixed intermediate resolution (~480×270) that the player bilinearly samples per cell. Avoid protobuf/flatbuffers ceremony — serde+zstd is the 80/20 call.

## 4. What already does 80%, honestly

- **notcurses `ncplayer` and mpv `--vo=tct`/`--vo=kitty` already do "video in a terminal at 30fps with graceful degradation."** If the goal were literal playback, we'd be done. **chafa** already does state-of-the-art glyph selection for stills.
- **Nobody does:** (1) resolution-independent precomputed asset + realtime glyph decision (every prior tool bakes grid size at render/encode time — resize = restart or artifact); (2) multi-layer artistic composition (base ramp ⊕ edge-orientation glyphs like `/|\-` ⊕ highlight ramp) — all prior art is single-signal luminance/color matching; (3) letterboxed aspect-locked "plasma screen" presentation; (4) glyph-palette tiers as *design* objects rather than blitter fallbacks; (5) a factory with quantitative quality metrics and CI-able golden/fuzz testing. These five are the differentiators; the terminal-I/O layer is a solved commodity we should keep thin.

License hygiene summary: safe deps = crossterm/ratatui/termion (MIT), notcurses (Apache, concepts + optional binding), hiptext (Apache, concepts). Concept-only (copyleft): chafa (LGPL), timg (GPL-2), ascii-live (GPL-3), mpv (GPL), jp2a (GPL).

Sources: [hiptext license (Apache-2.0)](https://github.com/jart/hiptext/blob/master/LICENSE), [ascii-live repo (GPL-3.0)](https://github.com/hugomd/ascii-live)
