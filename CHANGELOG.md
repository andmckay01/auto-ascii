# Changelog

## Unreleased

### Added

- **`AUTO_ASCII_FRAME_LOG=<path>`** makes the player append one row per
  presented frame (`frame_index`, `wall_ms`, `bytes`, `cells_damaged`,
  `write_ns`, `dropped`, tab-separated) and a final `# end …` summary line, to
  measure skipped frames and terminal back-pressure on a real tty. Unset, it
  costs nothing. See `docs/TERMINAL-CHECKLIST.md`.

### Changed

- **Apple Terminal.app is capped at 256 colors.** It renders 24-bit color
  (2.15, verified 2026-09-30), but truecolor costs more than it is worth there,
  and it received the truecolor stream whenever
  `COLORTERM=truecolor` was exported, which shell configs often do. The probe
  now keys on `TERM_PROGRAM=Apple_Terminal` and limits the tier to 256 after
  the passive hints, the volley and the cache, so neither `COLORTERM` nor a
  reply can lift it; `--tier truecolor` still opts back in. Measured on Terminal.app
  2.15 (M3 Pro, 30 fps clip): the 256 tier is about a third of the bytes and
  half the Terminal.app CPU of truecolor at the same grid (200×55: 43 against
  143 KB/frame, 80% against 146% of a core), and a 240×70 grid that skipped 5.6%
  of frames at truecolor skips none at 256 with `--repaint diff`.

### Fixed

- **Zooming the font while playing refits the picture.** The cell aspect
  (cell pixel height over width) was measured once at launch, so a zoom
  reflowed the picture with the old shape: a Ghostty session launched at 2pt
  (aspect 2.5) and zoomed to 7pt (8×18 px cells, aspect 2.25) drew it about
  10% too short, leaving 11 rows unused. The terminal backend now re-reads
  the cell pixel size on every resize, and `play` and `compose play` refit
  the picture whenever that changes the aspect, even at the same grid size,
  unless `--cell-aspect` pins it.

## 0.3.0 — 2026-09-29

One command. The `auto-ascii` binary now does everything the old player and
factory binaries did; `auto-ascii --help` leads with `play`, `stream` and
`import`, and `auto-ascii --help-all` shows every command and flag. It ships
from the `auto-ascii` crate, so `cargo install auto-ascii` installs it.

Versions: every published crate (`auto-ascii`, `auto-ascii-core`,
`auto-ascii-format`, `auto-ascii-term`, `auto-ascii-eval`,
`auto-ascii-factory`) now shares one workspace version, 0.3.0. Assets no
longer record the factory crate's version: META's `factory_version` is the
factory's `PIPELINE_VERSION` (still `0.1.0`), which moves only when the built
bytes do.

### Breaking

- **The `auto-ascii-player` and `auto-ascii-factory` binaries are removed**,
  with no shims or forwarding wrappers. Use `auto-ascii`:

  | before | now |
  |---|---|
  | `auto-ascii-player <asset \| comp.toml> [flags]` | `auto-ascii play <asset \| comp.toml> [flags]` (same flags) |
  | `auto-ascii-player … --sim COLSxROWS:N [--sim-tier/-dump/-resize/-audio]` | `auto-ascii play … --sim …`, or `auto-ascii dev sim <target> --sim …` |
  | `auto-ascii-player … --bench-seek N` | `auto-ascii play … --bench-seek N`, or `auto-ascii dev bench-seek <target> --bench-seek N` |
  | `auto-ascii-player --help` | `auto-ascii play --help-all` (`play --help` shows the everyday flags) |
  | `auto-ascii-factory build IN -o OUT [--ss --t --fps --res --params]` | `auto-ascii import IN -o OUT [same flags]`; add `--force` to replace an existing file |
  | `auto-ascii-factory inspect \| params \| eval \| sweep \| font-table …` | `auto-ascii dev inspect \| params \| eval \| sweep \| font-table …` (same flags) |
  | `auto-ascii-player --version`, `auto-ascii-factory --version` | `auto-ascii --version` |
  | `cargo build -p auto-ascii --features bin`, `cargo build -p auto-ascii-factory` | `cargo build -p auto-ascii` |
  | `cargo install auto-ascii` (installed the player) | `cargo install --path crates/auto-ascii` |

- The `auto-ascii` crate's `bin` feature and its optional `anyhow` dependency
  are gone. The binary now needs the new `cli` feature; default features are
  `terminal`, `audio`, `compose` and `cli`. Depend on the crate with
  `default-features = false` (adding `terminal`, `audio` or `compose` as
  needed) to get the lean library without the CLI's dependencies.
- **Codec-named compatibility is removed** (deprecated since the rename to
  "style"): the hidden `--codec` flag on `play`, `compose play`, `stream` and
  the `headless-dump` example; the `codec = "…"` key in a video's saved
  `<clip>.player.toml`, which is now ignored like any unknown key (the video
  plays in the default style until `s` saves a `style =` key);
  `auto_ascii::Codec`; `PlayerBuilder::codec`; `RenderSession::codec` /
  `set_codec`; and in `auto-ascii-core` the `codec` module, the root `Codec` /
  `GlyphCodec` re-exports, `compose_frame_codec` and
  `hysteresis::cell_flags::CODEC_PRIVATE_MASK`. Use `Style`, `GlyphStyle`,
  `compose_frame_style`, `STYLE_PRIVATE_MASK` and the `style` methods. The
  `.ascii` header's compression `codec` byte is unrelated and unchanged.
- `auto-ascii play` and `auto-ascii compose play` exit with status **3** when
  the viewer quits (q, Esc, Ctrl-C), as the old player did; before they exited
  0. 0 still means playback reached the end, 1 an error.
- `auto-ascii import -o PATH` refuses to overwrite an existing file unless
  `--force` is given (the old `factory build -o` overwrote silently).
- `scripts/play-with-sound.command` takes the `auto-ascii` binary instead of a
  player binary (`play-with-sound.command CLI ASSET [PLAY_ARGS…]`), runs
  `CLI play`, and no longer skips a legacy audio-file argument.
- `scripts/release.sh` is removed. Releases come from the tag-driven
  workflow (see Added), which ships one `auto-ascii` binary per target instead
  of `auto-ascii-player-<target>` plus an informational factory.
- Diagnostics that said `auto-ascii-player: sound: …` / `settings: …` now say
  `auto-ascii: …`; "not a valid ASCI asset" now reads "not a valid .ascii
  asset" (and the container's own errors say ".ascii file").
- `auto-ascii dev params` without `--dump` is a usage error (exit 2) rather
  than a runtime error.
- The factory library's `eval`, `sweep`, `reel` and `font_table` modules
  moved into the CLI as the `dev eval`, `dev sweep` and `dev font-table` code,
  so the factory no longer depends on `auto-ascii` and is published.
  `ffmpeg::Programs::lookup` is gone: `EvalArgs` and `SweepArgs` carry the
  resolved `programs`.
- Files a published crate embeds now live inside it:
  `crates/auto-ascii-factory/params.toml` (pinned to the repo-root
  `params.toml` by `factory_embeds_the_committed_params_file` in
  `crates/auto-ascii/tests/m2_params_eval.rs`) and
  `crates/auto-ascii/AGENT-GUIDE.md` (pinned to `docs/AGENT-GUIDE.md` by
  `agent_guide_prints_the_committed_file` in `crates/auto-ascii/tests/cli.rs`).

### Added

- `auto-ascii play` takes the full player flag set. Everyday flags (`--loop`,
  `--seek`, `--style`, `--palette`, `--mute`, `--no-audio`) are in
  `play --help`; the advanced ones (`--tier`, `--no-query`, `--no-cache`,
  `--no-quirks`, `--no-backdrop`, `--font-table`, `--repaint`, `--fps-cap`,
  `--cell-aspect`, `--duration-secs`, `--sim`, `--sim-tier`, `--sim-dump`,
  `--sim-resize`, `--sim-audio`, `--bench-seek`) are hidden but accepted.
  `compose play` takes the same flags.
- A global `--help-all` that shows every hidden flag, for one command
  (`auto-ascii play --help-all`) or for all of them (`auto-ascii --help-all`).
- `auto-ascii import -o PATH`: build to any path with no library folder,
  sidecar or registration; `--json` prints `{path, frames, fps,
  duration_secs, base_w, base_h, bytes}`. `import` also takes `--params FILE`
  in both modes, and rejects `--t 0`.
- `auto-ascii dev`: `inspect`, `params`, `eval`, `sweep`, `font-table`, `sim`
  and `bench-seek`, each printing one JSON value with `--json` (`dev inspect`
  gained a structured report for it).
- `play --sim` / `--bench-seek` (and `dev sim` / `dev bench-seek`) run with
  or without `--json` and print their one JSON line on stdout.
- The `headless-dump` example takes `--warm N` (render the N frames before
  `--from` without printing them) and `--cells` (print glyph, fg and bg per
  cell). `tools/readme_gif.py` uses them to render the README GIF.
- Tag-driven releases: pushing a `vX.Y.Z` tag runs
  `.github/workflows/release.yml`, which builds `auto-ascii` for six targets
  (aarch64/x86_64-apple-darwin, x86_64/aarch64-unknown-linux-gnu,
  x86_64/aarch64-pc-windows-msvc) and publishes the GitHub Release,
  crates.io, npm and the Homebrew tap. `docs/RELEASING.md` has the steps.
- `scripts/install.sh` (macOS, Linux) and `scripts/install.ps1` (Windows)
  install the prebuilt binary from a GitHub Release, and refuse to install a
  download whose SHA-256 does not match the published one.
- `brew install andmckay01/tap/auto-ascii`.
- `npm i -g auto-ascii` or `npx auto-ascii`: the `auto-ascii` npm package
  runs the prebuilt binary from its `@auto-ascii/<platform>` package.
- `cargo binstall auto-ascii` fetches the prebuilt binary
  (`[package.metadata.binstall]` in `crates/auto-ascii/Cargo.toml`).

### Changed

- `auto-ascii --help` is short and leads with play, stream and import;
  `stream --sim` / `--sim-dump` moved behind `--help-all`.
- `auto-ascii play <path>` plays an existing file without resolving a home
  folder first.
- `auto-ascii dev eval` / `dev sweep` resolve ffmpeg and ffprobe like `import`
  does (env override, PATH, the auto-ascii cache, a download on first use
  with consent) and hand the resolved paths to every ffmpeg subprocess they
  start.
- The generated font tables' header names `auto-ascii dev font-table`.
- `tools/soak.py` runs `auto-ascii play` and expects the quit status 3 after
  it presses `q`.
