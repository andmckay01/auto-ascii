#!/usr/bin/env bash
# M2 item F (PLAN §6/§7): the one-command loop — workspace tests + goldens +
# resize-fuzz fast mode + perf gate + corpus eval, fail-fast, < 5 min wall.
#
#   scripts/eval.sh
#
# Sections (each timed; the script aborts on the first failure):
#   1 tests    cargo test --workspace — includes the 36 insta cell-grid
#              goldens, the 4 per-tier escape-stream byte goldens, the
#              Player/FixtureRenderer parity pin, the 256-case resize fuzz
#              (real Player, M2 review fix), the >= 24 fps unthrottled
#              SimBackend e2e gate, and the synthetic determinism byte-pin.
#   2 clippy   cargo clippy --workspace --all-targets -- -D warnings.
#   3 fuzz     resize fuzz at FUZZ_CASES (default 2000) proptest cases.
#              Full M2 acceptance depth: FUZZ_CASES=10000 scripts/eval.sh
#              (~85 s extra on the reference box).
#   4 perf     scripts/perf-gate.sh — criterion medians vs the committed
#              perf/thresholds.toml.
#   5 corpus   only when corpus/ holds video files (gitignored, local-only):
#              `auto-ascii-factory eval --corpus corpus` against
#              runs/base.json (when present), writing runs/latest.{json,html}
#              and caching built assets under runs/cache.
#              Skipped with a notice otherwise — committed gates never
#              depend on the corpus (repo reproducibility rule).
#              The eval runs a RELEASE build (M3, wall-clock budget): a
#              debug eval pass ate a large slice of the <5 min budget;
#              factory output is byte-identical dev/release and metric math
#              is IEEE f64 (profile-independent), so release changes nothing
#              but the wall time.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

FUZZ_CASES="${FUZZ_CASES:-2000}"

TOTAL_START=$SECONDS
declare -a TIMES=()

section() { # section <name> — prints the header, starts the clock
    echo
    echo "==================== [$1] ===================="
    SECTION_NAME="$1"
    SECTION_START=$SECONDS
}
section_done() {
    local dt=$((SECONDS - SECTION_START))
    TIMES+=("$(printf '%-22s %4ds' "$SECTION_NAME" "$dt")")
    echo "-------------------- [$SECTION_NAME] OK (${dt}s)"
}

section "workspace tests"
cargo test --workspace --quiet
section_done

section "clippy -D warnings"
cargo clippy --workspace --all-targets --quiet -- -D warnings
section_done

section "resize fuzz x$FUZZ_CASES"
PROPTEST_CASES="$FUZZ_CASES" cargo test --quiet -p auto-ascii --test resize_fuzz
section_done

section "perf gate"
scripts/perf-gate.sh
section_done

# The corpus is whatever video files sit directly in corpus/ (gitignored,
# local-only) — the same non-recursive extension set the factory's `eval`
# scans for (VIDEO_EXTS, crates/auto-ascii-factory/src/eval.rs).
corpus_clips=()
shopt -s nullglob nocaseglob
for clip in corpus/*.mp4 corpus/*.mov corpus/*.mkv corpus/*.webm corpus/*.avi corpus/*.m4v; do
    if [[ -f "$clip" ]]; then corpus_clips+=("$clip"); fi
done
shopt -u nullglob nocaseglob

if (( ${#corpus_clips[@]} > 0 )); then
    section "corpus eval"
    # The baseline is local, never committed: record it once with
    # `auto-ascii-factory eval --corpus corpus --out runs/base.json`
    # (corpus/README.md). EVAL_BASELINE points one run at another file;
    # with no baseline present the eval still runs, without the compare.
    baseline="${EVAL_BASELINE:-runs/base.json}"
    baseline_args=()
    if [[ -f "$baseline" ]]; then
        baseline_args=(--baseline "$baseline")
    else
        echo "NOTICE: $baseline missing — running eval without baseline compare"
    fi
    # eval creates runs/ itself: --cache-dir and the --out/--html parents.
    # The +-expansion is the bash 3.2 (macOS /bin/bash) safe form: a plain
    # "${arr[@]}" on an EMPTY array is an unbound variable there under set -u,
    # and no-baseline is the normal case on a checkout without runs/base.json.
    cargo run --quiet --release -p auto-ascii-factory -- eval \
        --corpus corpus ${baseline_args[@]+"${baseline_args[@]}"} \
        --out runs/latest.json --html runs/latest.html --cache-dir runs/cache
    section_done
else
    echo
    echo "==================== [corpus eval] ===================="
    echo "NOTICE: no video files in corpus/ (local-only, gitignored) —"
    echo "skipping corpus eval. Synthetic goldens, fuzz and perf gates above"
    echo "are the corpus-free CI surface."
    TIMES+=("$(printf '%-22s %4s' 'corpus eval' 'skip')")
fi

echo
echo "==================== eval.sh: ALL GREEN ===================="
for t in "${TIMES[@]}"; do echo "  $t"; done
printf '  %-22s %4ds\n' "TOTAL" $((SECONDS - TOTAL_START))
