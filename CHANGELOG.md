# Changelog

## 0.3.0 (unreleased)

One command. The `auto-ascii` binary now does everything the old player and
factory binaries did; `auto-ascii --help` leads with `play`, `stream` and
`import`, and `auto-ascii --help-all` shows every command and flag.

Versions: `auto-ascii` 0.2.0 → 0.3.0, `auto-ascii-core` 0.1.0 → 0.2.0,
`auto-ascii-cli` 0.2.0 (unpublished). `auto-ascii-factory` keeps its version
(it is recorded in every asset's META), and is now `publish = false`.

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
  | `cargo build -p auto-ascii --features bin`, `cargo build -p auto-ascii-factory` | `cargo build -p auto-ascii-cli` |
  | `cargo install auto-ascii` (installed the player) | `cargo install --path crates/auto-ascii-cli` |

- The `auto-ascii` library crate ships no binary: the `bin` feature and its
  optional `clap` / `anyhow` dependencies are gone. Default features are now
  `terminal`, `audio` and `compose`, so the library API a default build gets is
  unchanged.
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
- `scripts/release.sh` builds and ships one `auto-ascii-<target>` binary per
  target instead of `auto-ascii-player-<target>` plus an informational factory.
- Diagnostics that said `auto-ascii-player: sound: …` / `settings: …` now say
  `auto-ascii: …`; "not a valid ASCI asset" now reads "not a valid .ascii
  asset" (and the container's own errors say ".ascii file").
- `auto-ascii dev params` without `--dump` is a usage error (exit 2) rather
  than a runtime error.
- In the unpublished factory library, `ffmpeg::Programs::lookup` is gone:
  `eval::EvalArgs` and `sweep::SweepArgs` carry the resolved `programs`.

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
