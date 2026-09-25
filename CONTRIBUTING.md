# Contributing

auto-ascii is a Rust workspace (edition 2024). This page is the working
agreement: how to build, what "green" means, and the few rules that keep the
renderer deterministic. The design lives in [docs/PLAN.md](docs/PLAN.md) (the
engine) and [docs/PLAN-M6-M8.md](docs/PLAN-M6-M8.md) (the CLI and
compositions); [docs/INTERFACES.md](docs/INTERFACES.md) is the internal API
registry, milestone by milestone.

## Build and run

```bash
cargo build --release -p auto-ascii --features bin   # the player (or: make build)
cargo build --release -p auto-ascii-factory          # the factory; ffmpeg on PATH to ingest
cargo build --release -p auto-ascii-cli              # the `auto-ascii` CLI
cargo test --workspace                               # or: make test
```

macOS builds from source on Apple Silicon and Intel; there is no cross build
for it. The player needs only crossterm and POSIX termios, and the factory
needs `ffmpeg` on PATH (`brew install ffmpeg`) for ingest only.

`scripts/release.sh` (or `make dist`) is Linux-hosted. It builds stripped
player binaries into `dist/` for `x86_64-unknown-linux-gnu` (native),
`x86_64-unknown-linux-musl` (fully static, checked with `ldd`) and
`x86_64-pc-windows-gnu` (MinGW cross; smoke-tested under wine only when wine
is installed), and gates each under 5 MB. It also reports the factory's size,
ungated. Missing toolchains are installed with `rustup target add` and
`sudo -n apt-get install` (musl-tools, mingw-w64); `NO_APT=1` forbids apt and
fails instead.

## The gate

`scripts/eval.sh` is what "green" means, and it must end in `ALL GREEN`:

1. `cargo test --workspace` — including the insta cell-grid goldens, the
   per-tier escape-stream goldens, the Linux console golden, the pipeline
   parity pin and the factory's byte-pin determinism test;
2. `cargo clippy --workspace --all-targets -- -D warnings`;
3. the resize fuzz (`FUZZ_CASES`, default 2000; `FUZZ_CASES=10000
   scripts/eval.sh` is the full depth);
4. the criterion perf gate (`scripts/perf-gate.sh` against
   `perf/thresholds.toml`);
5. a corpus eval, only when `corpus/` holds local videos
   ([corpus/README.md](corpus/README.md)). It runs a release build of
   `auto-ascii-factory eval` against `runs/base.json` when that file exists
   (`EVAL_BASELINE=path` points it elsewhere), writing
   `runs/latest.{json,html}` and caching assets under `runs/cache`. Record a
   baseline once with `auto-ascii-factory eval --corpus corpus --out
   runs/base.json`.

The script aborts on the first failing section and prints each section's
time.

Nothing committed depends on the corpus: synthetic fixtures in
`auto-ascii-eval` back every golden, fuzz and perf check.

## Rules that keep the output deterministic

- **Assets never store glyphs.** Glyph choice happens at render time; that is
  what makes resize correct. Planes only (docs/PLAN.md §4).
- **Goldens and byte pins move only deliberately.** The insta snapshots, the
  `.ansi` tier goldens, the console golden, `FIXTURE_ASSET_SHA` in the
  factory tests and `GOLDEN_SHA256` in the format tests are the regression
  baseline. Re-pin with the reason in the commit and a visual spot-check
  through the eval rasterizer, and run `cargo test --no-fail-fast` when
  hunting pins, because cargo stops at the first failing target.
- **`params.toml` is the only home for tunables.** The factory embeds it as
  its defaults, so editing the build-side tables (`[build]`, `[shots]`,
  `[levels]`, `[edges]`, `[highlights]`, `[temporal]`) is a re-baselining
  act: the byte-pin test and any `runs/base.json` you keep will move.
  Glyph-codec design constants — the `letters` ramps and its fill/hold
  thresholds, like the §3.4 palettes — are codec *data*, not tunables: they
  live in their codec module (`auto-ascii-core/src/codec/`), pinned by its
  tests and goldens. Anything a viewer or a sweep adjusts is a
  `ComposeParams` field in `[compose]`, which every codec reads.
- **Perf thresholds are calibrated, not guessed.** Each `perf/thresholds.toml`
  entry's `max_median_ns` is the median of medians over three consecutive
  runs × 1.15 on an idle machine; `measured_median_ns` is informational.
  The committed values were calibrated on the reference box: 2 physical
  cores / 4 logical (EPYC-Milan), Linux, Rust 1.97.1. Recalibrate both
  fields of an entry together, on that box, never from a single run on a
  busy machine. ×1.15 stays below a deliberate 20% regression, so such a
  regression always trips the gate. The gate also fails when a bench
  disappears, when a bench's estimates were not refreshed by this run
  (renamed bench), or when a new bench has no threshold, so renaming a
  bench means editing that file. `scripts/perf-gate.sh --no-run` compares
  existing estimates without re-running the benches (existence, thresholds
  and coverage only; staleness needs a run).
- **The player stays codec-free and rayon-free.** (Video codecs — decoding
  is the factory's job. The *glyph* codecs in `auto_ascii_core::codec`,
  which map cell features to glyphs at render time, are not what this means.)
  `cargo tree -p auto-ascii -e normal | grep -c rayon` is 0, and a
  `--no-default-features` build carries no clap, anyhow or crossterm.
  (`rayon` appears in the workspace only behind `auto-ascii-format`'s
  optional, off-by-default `parallel` feature, which compresses a frame's
  plane subblocks in parallel for the factory.)
- **Overlays are event-driven.** No always-on chrome in the player; the parity
  and console-golden tests render the real grid and must keep passing
  unblessed.
- **No connectivity engineering.** No SSH/WAN tuning, tmux/ConPTY special
  cases or throughput governors (docs/PLAN.md, scope amendment at the top).

## Layout

| path | what |
|---|---|
| `crates/auto-ascii` | the public library + the `auto-ascii-player` binary |
| `crates/auto-ascii-cli` | the `auto-ascii` CLI (unpublished; depends on the factory) |
| `crates/auto-ascii-factory` | offline factory, eval and sweep drivers (unpublished; lib + bin) |
| `crates/auto-ascii-core` / `-format` / `-term` | engine, container, terminal backend |
| `crates/auto-ascii-eval` | metrics, fixtures, report schema (unpublished) |
| `params.toml`, `perf/thresholds.toml` | the tunables and the perf gates |
| `scripts/` | `eval.sh` (the gate), `perf-gate.sh`, `release.sh` |
| `tools/` | `prep_video.py`, `soak.py` |
| `corpus/`, `runs/` | local videos and eval output; both gitignored |
| `docs/` | feature map, plans, API registry, agent guide, terminal checklist, research digests |

## Publishing

`auto-ascii` (0.2.x) and `auto-ascii-core`, `auto-ascii-format`,
`auto-ascii-term` (0.1.x) are on crates.io; the factory, eval and CLI crates
are not published. The facade's version is ahead of the libraries' because
`auto-ascii` 0.1.0 was published under the project's earlier crate names and
is yanked. Publish in the order core, format, term, facade, bumping versions
first: a published version number can never be reused. Path dependencies
carry a version requirement alongside the path so `cargo package` can
rewrite them; `auto-ascii-eval` is deliberately path-only, because it is a
dev-dependency (it dev-dep-cycles with `auto-ascii-term`) and cargo strips
path-only dev-dependencies when packaging.

The workspace allows one clippy lint, `chunks_exact_to_as_chunks`
(`Cargo.toml` `[workspace.lints.clippy]`): it would rewrite about twenty
hot `chunks_exact(N)` loops into equivalent code, which is not worth
churning the perf gate over. The `npm/` package is
a name placeholder only.

The project is MIT licensed ([LICENSE](LICENSE)); contributions are accepted
under the same terms.
