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
3. the failing comment rule, which reports counts and violations;
4. the resize fuzz (`FUZZ_CASES`, default 2000; `FUZZ_CASES=10000
   scripts/eval.sh` is the full depth);
5. the criterion perf gate (`scripts/perf-gate.sh` against
   `perf/thresholds.toml`);
6. a corpus eval, only when `corpus/` holds local videos
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

## Comment rule

A source file may have one optional leading ownership paragraph, at most
five text lines. Rust uses adjacent `//!` lines; shell, Python, TOML,
Makefile and `.gitignore` use adjacent `#` lines. A BOM and leading whitespace
may precede the header. In shell, Python and Rust scripts, an interpreter
shebang (`#!/path` or `#! /path`) may also precede it. A `#!` line in TOML,
Makefiles or `.gitignore` is an ordinary comment. Rust inner attributes
may follow it, but cannot precede it. Delimiter-only outer lines do not
count; a blank comment line inside the paragraph fails. A blank source
line separates clusters, so a later block is a second header and fails.
Ownership versus justification, banners and document pointers remain
review judgments; the checker enforces syntax, position and size.

Every other source comment is forbidden, including trailing comments,
Rust item/block docs, prose `doc = ...` attributes (also inside `cfg_attr`
and literal macro bodies), and Python module/class/function docstrings.
Non-prose metadata such as `#[doc(hidden)]` and lint reasons inside
`#[expect(..., reason = "...")]` are allowed. Express knowledge in code
first: names, constants, types, assertions and tests. Facts code cannot
carry belong in [docs/NOTES.md](docs/NOTES.md); code must not point there.
Preserve load-bearing doctests as real tests/examples during cleanup.

The unpublished `auto-ascii-lint` crate provides `check-comments`. It uses
rustc's lexer for Rust, Tree-sitter grammars for shell and TOML, Python's
standard tokenizer and AST for Python, and narrow Make/gitignore scanners.
This preserves literal text, including raw/byte/C strings, generated TOML,
shell expansions and heredocs, without maintaining a second Rust lexer.
Make assignments retain Make comment syntax; `.ONESHELL` recipes preserve
shell context across blank lines and inline first commands.
Development checks require `python3` 3.12+ on PATH; no pip install is needed.
Dependencies and extraction boundaries are recorded in
[Technology notes](docs/NOTES.md#technology).

```sh
make comments
make lint
cargo run --quiet --release -p auto-ascii-lint --bin check-comments -- --paths crates/auto-ascii-lint scripts
cargo run --quiet --release -p auto-ascii-lint --bin check-comments -- --count
```

Scope is tracked plus untracked, nonignored `.rs`, `.sh`, `.py`, `.toml`,
Makefiles and `.gitignore` files. Cargo.lock is generated dependency data
and excluded, as are Markdown, JSON, goldens, snapshots, LICENSE and other
extensions. Credential-like paths are never read; scoped source symlinks
are refused, including symlinked parent directories below the repository
root. The same restriction applies to the allowlist. `--paths` selects
repository-relative files or directories, with directory boundaries, and
scopes allowlist staleness too.

`scripts/comment-allowlist.toml` starts empty (`entries = []`). Its only
entry fields are `path`, exact `comment` cluster text, and a nonempty
`reason`. Unknown/missing fields, duplicate entries and stale exemptions
fail; an already-compliant header needs no exemption and makes an entry
stale. There is no README doc-attribute exception or whole-file exemption.

Default output is `file:line: reason` per violating cluster. `--count`
instead prints scanned/commented files, clusters and physical comment
lines by crate/area, including compliant headers and blank comment lines.
Both print violation totals. Exit codes are 0 for success, 1 for policy or
staleness failures, and 2 for configuration/extraction errors.
`--report-only` suppresses only the policy failure status; broken scans
and stale exemptions still fail.

The `comment rule` section of `scripts/eval.sh`, `make comments`, and
`make lint` all fail on violations or stale exemptions. Workspace tests
cover language fixtures and use isolated repositories for CLI regressions
and the tooling's own compliance. The eval gate scans the live worktree.
The repo has no `.github` CI or pre-commit hook; enforcement is in the
existing eval gate.

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
| `crates/auto-ascii-lint` | comment policy checker and fixtures (unpublished) |
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
