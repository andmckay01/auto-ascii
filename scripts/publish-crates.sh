#!/usr/bin/env bash
# Publishes the workspace crates to crates.io in dependency order, skipping any
# whose version is already there, so a re-run resumes after a partial failure.
# Needs CARGO_REGISTRY_TOKEN. DRY_RUN=1 packages and verifies every crate in one
# cargo invocation, which resolves each against the others' local packages
# before any is on crates.io, and uploads nothing.
set -euo pipefail
cd "$(dirname "$0")/.."

CRATES=(
    auto-ascii-format
    auto-ascii-core
    auto-ascii-term
    auto-ascii-eval
    auto-ascii-factory
    auto-ascii
)
USER_AGENT="auto-ascii-release (https://github.com/andmckay01/auto-ascii)"

metadata=$(cargo metadata --no-deps --format-version 1)
version=$(jq -r '.packages[] | select(.name=="auto-ascii") | .version' <<<"$metadata")
[ -n "$version" ] || { echo "publish-crates: no auto-ascii package in cargo metadata" >&2; exit 1; }

for name in "${CRATES[@]}"; do
    crate_version=$(jq -r --arg n "$name" '.packages[] | select(.name==$n) | .version' <<<"$metadata")
    if [ "$crate_version" != "$version" ]; then
        echo "publish-crates: $name is at '$crate_version', not the workspace version $version" >&2
        exit 1
    fi
done

if [ "${DRY_RUN:-0}" = 1 ]; then
    packages=()
    for name in "${CRATES[@]}"; do
        packages+=(-p "$name")
    done
    cargo publish --locked --dry-run --allow-dirty "${packages[@]}"
    exit 0
fi

for name in "${CRATES[@]}"; do
    status=$(curl -sS --retry 3 -o /dev/null -w '%{http_code}' \
        -H "User-Agent: $USER_AGENT" \
        "https://crates.io/api/v1/crates/$name/$version")
    if [ "$status" = 200 ]; then
        echo "skip $name $version (already published)"
        continue
    fi
    printf '\n== cargo publish -p %s %s\n' "$name" "$version"
    cargo publish -p "$name" --locked
done
