#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

FUZZ_CASES="${FUZZ_CASES:-2000}"

TOTAL_START=$SECONDS
declare -a TIMES=()

section() {
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

corpus_clips=()
shopt -s nullglob nocaseglob
for clip in corpus/*.mp4 corpus/*.mov corpus/*.mkv corpus/*.webm corpus/*.avi corpus/*.m4v; do
    if [[ -f "$clip" ]]; then corpus_clips+=("$clip"); fi
done
shopt -u nullglob nocaseglob

if (( ${#corpus_clips[@]} > 0 )); then
    section "corpus eval"
    baseline="${EVAL_BASELINE:-runs/base.json}"
    baseline_args=()
    if [[ -f "$baseline" ]]; then
        baseline_args=(--baseline "$baseline")
    else
        echo "NOTICE: $baseline missing — running eval without baseline compare"
    fi
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
