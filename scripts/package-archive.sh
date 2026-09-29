#!/usr/bin/env bash
# Usage: scripts/package-archive.sh <target> <version> <tar.gz|zip>
# Packs target/<target>/release/auto-ascii[.exe], stripped in place except on
# Windows, flat with LICENSE and README.md into dist/auto-ascii-<target>.<ext>,
# and writes "<hex>  <file>" to its .sha256 on every platform, never Git Bash's
# binary-mode "*". The release workflow runs it once per build target.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ $# -ne 3 ]; then
    echo "usage: $0 <target> <version> <tar.gz|zip>" >&2
    exit 2
fi
target=$1
version=$2
ext=$3

case "$target" in
    *-windows-*) exe=auto-ascii.exe ;;
    *) exe=auto-ascii ;;
esac
bin="target/$target/release/$exe"
[ -f "$bin" ] || { echo "package-archive: $bin not found; build --target $target first" >&2; exit 1; }

case "$target" in
    *-windows-*) ;;
    *) strip "$bin" ;;
esac

mkdir -p dist
name="auto-ascii-$target.$ext"
archive="$PWD/dist/$name"
rm -f "$archive" "$archive.sha256"

stage=$(mktemp -d "${TMPDIR:-/tmp}/auto-ascii-package.XXXXXX")
trap 'rm -rf "$stage"' EXIT
cp "$bin" LICENSE README.md "$stage/"
files=("$exe" LICENSE README.md)

case "$ext" in
    tar.gz)
        COPYFILE_DISABLE=1 tar -C "$stage" -czf "$archive" "${files[@]}"
        ;;
    zip)
        if command -v 7z >/dev/null 2>&1; then
            (cd "$stage" && 7z a -tzip -mx=9 -bso0 -bsp0 "$archive" "${files[@]}")
        elif command -v zip >/dev/null 2>&1; then
            (cd "$stage" && zip -9 -q "$archive" "${files[@]}")
        elif command -v powershell.exe >/dev/null 2>&1; then
            powershell.exe -NoProfile -Command \
                "Compress-Archive -Path '$(cygpath -w "$stage")\\*' -DestinationPath '$(cygpath -w "$archive")'"
        else
            echo "package-archive: need 7z, zip or powershell.exe to write $name" >&2
            exit 1
        fi
        ;;
    *)
        echo "package-archive: unknown archive type $ext (want tar.gz or zip)" >&2
        exit 2
        ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
    hex=$(sha256sum "$archive" | cut -d' ' -f1)
else
    hex=$(shasum -a 256 "$archive" | cut -d' ' -f1)
fi
printf '%s  %s\n' "$hex" "$name" > "$archive.sha256"

sz=$(wc -c < "$archive" | tr -d ' ')
printf 'auto-ascii %s %s: dist/%s %d bytes (%d KiB)\n' "$version" "$target" "$name" "$sz" $((sz / 1024))
