#!/usr/bin/env bash
# M2 item E (PLAN §6): the criterion perf gate.
#
# Runs the auto-ascii pipeline benches (decode / resample / compose /
# present truecolor+256 / end-to-end frame), then compares each criterion
# median (target/criterion/<id>/new/estimates.json) against the committed
# thresholds in perf/thresholds.toml. Exits nonzero on any breach.
#
# Coverage rules (M2-low fix c — the gate must not silently shrink):
#   * a thresholds.toml entry with NO estimates.json fails (bench deleted);
#   * after a bench run, an entry whose estimates were NOT refreshed by that
#     run fails as STALE — the classic renamed-bench trap, where old
#     estimates would otherwise keep passing forever;
#   * a bench that produced estimates but has NO thresholds.toml entry fails
#     (new/renamed bench must be added to the committed gate).
#
# Usage: scripts/perf-gate.sh [--no-run]
#   --no-run   skip `cargo bench` and only compare existing estimates
#              (e.g. right after a manual bench run). Staleness cannot be
#              judged without a run marker, so --no-run checks existence,
#              thresholds and coverage over whatever estimates exist.
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

MARKER=""
if [[ "${1:-}" != "--no-run" ]]; then
    MARKER="target/.perf-gate-run-start"
    mkdir -p target
    touch "$MARKER"
    cargo bench -p auto-ascii --bench pipeline -- --noplot
fi

GATE_MARKER="$MARKER" python3 - <<'PY'
import json
import os
import pathlib
import sys
import tomllib

thresholds = tomllib.load(open("perf/thresholds.toml", "rb"))["bench"]
marker = os.environ.get("GATE_MARKER") or None
run_started = os.path.getmtime(marker) if marker else None

# Every bench that has produced estimates, keyed by criterion bench id
# (directory path under target/criterion, minus the /new/estimates.json).
root = pathlib.Path("target/criterion")
estimates = {}
if root.is_dir():
    for p in sorted(root.rglob("new/estimates.json")):
        bench_id = p.parent.parent.relative_to(root).as_posix()
        if bench_id == "report":
            continue
        estimates[bench_id] = p

def fresh(path):
    """Was this estimates.json (re)written by THIS gate's bench run?"""
    return run_started is None or path.stat().st_mtime >= run_started

rows, fail = [], False
for name, t in thresholds.items():
    limit = t["max_median_ns"]
    path = estimates.get(name)
    if path is None:
        rows.append((name, None, limit, "FAIL (missing estimates.json — bench deleted/renamed?)"))
        fail = True
        continue
    if not fresh(path):
        rows.append((name, None, limit,
                     "FAIL (stale estimates — this run produced none; bench renamed?)"))
        fail = True
        continue
    median = json.load(open(path))["median"]["point_estimate"]
    verdict = "ok" if median <= limit else "FAIL"
    fail |= median > limit
    rows.append((name, median, limit, verdict))

# Coverage: benches that ran (fresh estimates) but are not in the committed
# gate. In --no-run mode every existing estimate dir is checked — delete
# stray dirs (old drills/renames) or add entries deliberately.
for name, path in estimates.items():
    if name in thresholds or not fresh(path):
        continue
    median = json.load(open(path))["median"]["point_estimate"]
    rows.append((name, median, None,
                 "FAIL (no perf/thresholds.toml entry — gate coverage must not shrink)"))
    fail = True

w = max(len(r[0]) for r in rows)
print(f"\n{'bench':<{w}}  {'median_ns':>12}  {'max_ns':>12}  {'headroom':>8}  verdict")
for name, median, limit, verdict in rows:
    m = f"{median:>12.0f}" if median is not None else f"{'—':>12}"
    l = f"{limit:>12}" if limit is not None else f"{'—':>12}"
    h = f"{1 - median / limit:>7.1%}" if median is not None and limit else f"{'—':>8}"
    print(f"{name:<{w}}  {m}  {l}  {h}  {verdict}")

if fail:
    print("\nperf gate: FAIL — see verdicts above (thresholds: perf/thresholds.toml; "
          "recalibrate only deliberately, on the reference box)")
    sys.exit(1)
print("\nperf gate: PASS")
PY
