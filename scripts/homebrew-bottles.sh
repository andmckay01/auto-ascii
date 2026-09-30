#!/usr/bin/env bash
# Usage: scripts/homebrew-bottles.sh <version> <dist-dir>
# Repacks the macOS and Linux release archives in <dist-dir> as Homebrew
# bottles, dist/auto-ascii-<version>.<tag>.bottle.tar.gz each with a .sha256,
# so brew pours the prebuilt binary instead of vetting a compiler toolchain.
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <version> <dist-dir>" >&2
    exit 2
fi
version=${1#v}
dist=$(cd "$2" && pwd)

bottle_targets=(
    aarch64-apple-darwin:arm64_big_sur
    x86_64-apple-darwin:big_sur
    aarch64-unknown-linux-gnu:arm64_linux
    x86_64-unknown-linux-gnu:x86_64_linux
)

sha256_hex() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

stage=$(mktemp -d "${TMPDIR:-/tmp}/auto-ascii-bottle.XXXXXX")
trap 'rm -rf "$stage"' EXIT

for pair in "${bottle_targets[@]}"; do
    target=${pair%%:*}
    tag=${pair#*:}
    archive="$dist/auto-ascii-$target.tar.gz"
    [ -f "$archive" ] || { echo "homebrew-bottles: missing $archive" >&2; exit 1; }

    keg="$stage/$tag/auto-ascii/$version"
    mkdir -p "$keg/bin"
    tar -xzf "$archive" -C "$keg" LICENSE README.md auto-ascii
    mv "$keg/auto-ascii" "$keg/bin/auto-ascii"
    chmod 0755 "$keg/bin/auto-ascii"

    name="auto-ascii-$version.$tag.bottle.tar.gz"
    rm -f "$dist/$name" "$dist/$name.sha256"
    COPYFILE_DISABLE=1 tar -C "$stage/$tag" -czf "$dist/$name" auto-ascii
    printf '%s  %s\n' "$(sha256_hex "$dist/$name")" "$name" > "$dist/$name.sha256"
    echo "auto-ascii $version $tag: $dist/$name"
done
