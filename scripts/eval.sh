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
#   5 corpus   only when the canonical 3-clip corpus (gitignored, local) is
#              present: assemble target/eval-corpus symlinks, run
#              `sleepy-factory eval` against runs/base.json (when present)
#              writing runs/latest.{json,html}, with the real-corpus
#              determinism guard (grass rebuild byte-identical to assets/)
#              running concurrently — byte-identity is load-invariant, so
#              the overlap only saves wall time (M3 Tune finish).
#              Skipped with a notice otherwise — committed gates never
#              depend on the corpus (repo reproducibility rule).
#              Both corpus steps run RELEASE builds (M3, wall-clock budget):
#              debug eval + debug grass rebuild alone ate ~150 s of the
#              <5 min budget; factory output is byte-identical dev/release
#              (verified at M3 integration and re-checked by the guard
#              itself every run) and metric math is IEEE f64 (profile-
#              independent), so release changes nothing but the wall time.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

FUZZ_CASES="${FUZZ_CASES:-2000}"

# Canonical eval corpus (corpus/README.md): the three locked clips.
CORPUS_CLIPS=(
    "corpus/prepared/grass-field-windy-mirror.mp4"
    "corpus/prepared/sheep-counting-neroni-clips.mp4"
    "corpus/silhouette-dance.mp4"
)

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
PROPTEST_CASES="$FUZZ_CASES" cargo test --quiet -p sleepytime --test resize_fuzz
section_done

section "perf gate"
scripts/perf-gate.sh
section_done

have_corpus=true
for clip in "${CORPUS_CLIPS[@]}"; do
    [[ -f "$clip" ]] || have_corpus=false
done

if $have_corpus; then
    section "corpus eval + determinism guard"
    # Non-recursive symlink dir: the canonical 3 clips only (corpus/prepared
    # also holds prep-tool variants that are not part of the baseline).
    corpus_dir="target/eval-corpus"
    rm -rf "$corpus_dir" && mkdir -p "$corpus_dir"
    for clip in "${CORPUS_CLIPS[@]}"; do
        ln -s "$(pwd)/$clip" "$corpus_dir/$(basename "$clip")"
    done
    # The determinism guard (release grass rebuild, byte-compared against
    # assets/) runs CONCURRENTLY with the eval pass (M3 Tune finish: the two
    # serially put the script at 302 s — over the <5 min M2 acceptance).
    # This overlap is sound: the guard's output is a temp-dir asset compared
    # by byte-identity — a load-invariant check (unlike the criterion perf
    # gate above, which stays isolated) — and the two share no artifacts
    # (guard builds fresh; eval reads runs/cache). cargo's target-dir lock
    # only serializes their already-warm no-op build steps.
    guard_log="target/eval-guard.log"
    cargo test --quiet --release -p sleepy-factory --test m2_params_eval -- \
        --ignored grass_rebuild_matches_assets_copy >"$guard_log" 2>&1 &
    guard_pid=$!
    # EVAL_BASELINE overrides the committed baseline for one run (default
    # runs/base.json — since the M3 Tune finish that IS the tuned M3
    # renderer baseline, schema v2 with the edge-F1 family; the M1-era
    # baseline it replaced is unreproducible by design after the deliberate
    # M3 renderer change). Keep the override for the next generational
    # transition; the committed default stays runs/base.json.
    baseline="${EVAL_BASELINE:-runs/base.json}"
    baseline_args=()
    if [[ -f "$baseline" ]]; then
        baseline_args=(--baseline "$baseline")
    else
        echo "NOTICE: $baseline missing — running eval without baseline compare"
    fi
    if ! cargo run --quiet --release -p sleepy-factory -- eval \
        --corpus "$corpus_dir" "${baseline_args[@]}" \
        --out runs/latest.json --html runs/latest.html --cache-dir runs/cache
    then
        # set -e is suspended inside `if`; keep fail-fast semantics but
        # never leave the guard orphaned.
        wait "$guard_pid" || true
        cat "$guard_log"
        exit 1
    fi
    guard_ok=true
    wait "$guard_pid" || guard_ok=false
    cat "$guard_log"
    $guard_ok || { echo "corpus determinism guard FAILED"; exit 1; }
    section_done
else
    echo
    echo "==================== [corpus eval] ===================="
    echo "NOTICE: corpus clips not present (gitignored, local-only) — skipping"
    echo "corpus eval + real-corpus determinism guard. Synthetic goldens,"
    echo "fuzz and perf gates above are the corpus-free CI surface."
    TIMES+=("$(printf '%-22s %4s' 'corpus eval' 'skip')")
fi

echo
echo "==================== eval.sh: ALL GREEN ===================="
for t in "${TIMES[@]}"; do echo "  $t"; done
printf '  %-22s %4ds\n' "TOTAL" $((SECONDS - TOTAL_START))
