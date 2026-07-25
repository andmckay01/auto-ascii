#!/usr/bin/env bash
# M2 item E (PLAN §6): the criterion perf gate.
#
# Runs the sleepy-player pipeline benches (decode / resample / compose /
# present truecolor+256 / end-to-end frame), then compares each criterion
# median (target/criterion/<id>/new/estimates.json) against the committed
# thresholds in perf/thresholds.toml. Exits nonzero on any breach or any
# missing estimate (gate coverage must not silently shrink).
#
# Usage: scripts/perf-gate.sh [--no-run]
#   --no-run   skip `cargo bench` and only compare existing estimates
#              (e.g. right after a manual bench run).
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v cargo >/dev/null 2>&1; then
    export PATH="$HOME/.cargo/bin:$PATH"
fi

if [[ "${1:-}" != "--no-run" ]]; then
    cargo bench -p sleepy-player --bench pipeline -- --noplot
fi

python3 - <<'PY'
import json
import pathlib
import sys
import tomllib

thresholds = tomllib.load(open("perf/thresholds.toml", "rb"))["bench"]
rows, fail = [], False
for name, t in thresholds.items():
    limit = t["max_median_ns"]
    path = pathlib.Path("target/criterion") / name / "new" / "estimates.json"
    if not path.exists():
        rows.append((name, None, limit))
        fail = True
        continue
    median = json.load(open(path))["median"]["point_estimate"]
    rows.append((name, median, limit))
    fail |= median > limit

w = max(len(r[0]) for r in rows)
print(f"\n{'bench':<{w}}  {'median_ns':>12}  {'max_ns':>12}  {'headroom':>8}  verdict")
for name, median, limit in rows:
    if median is None:
        print(f"{name:<{w}}  {'—':>12}  {limit:>12}  {'—':>8}  FAIL (missing estimates.json)")
        continue
    verdict = "ok" if median <= limit else "FAIL"
    print(f"{name:<{w}}  {median:>12.0f}  {limit:>12}  {1 - median / limit:>7.1%}  {verdict}")

if fail:
    print("\nperf gate: FAIL — median above committed threshold "
          "(perf/thresholds.toml; recalibrate only deliberately, on the reference box)")
    sys.exit(1)
print("\nperf gate: PASS")
PY
