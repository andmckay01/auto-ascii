#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

DIST=dist
MAX_PLAYER_BYTES=$((5 * 1024 * 1024))
MUSL_TARGET=x86_64-unknown-linux-musl
WIN_TARGET=x86_64-pc-windows-gnu
NATIVE_TARGET=$(rustc -vV | sed -n 's/^host: //p')

say() { printf '\n== %s\n' "$*"; }

say "prerequisites"
if ! command -v musl-gcc >/dev/null 2>&1; then
    [ "${NO_APT:-0}" = 1 ] && { echo "musl-gcc missing and NO_APT=1"; exit 1; }
    sudo -n apt-get install -y musl-tools
fi
if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    [ "${NO_APT:-0}" = 1 ] && { echo "mingw gcc missing and NO_APT=1"; exit 1; }
    sudo -n apt-get install -y mingw-w64
fi
rustup target add "$MUSL_TARGET" "$WIN_TARGET" >/dev/null

mkdir -p "$DIST"

say "build: native ($NATIVE_TARGET)"
cargo build --release -p auto-ascii --features bin
cp target/release/auto-ascii-player "$DIST/auto-ascii-player-$NATIVE_TARGET"

say "build: $MUSL_TARGET (fully static)"
cargo build --release --target "$MUSL_TARGET" -p auto-ascii --features bin
cp "target/$MUSL_TARGET/release/auto-ascii-player" "$DIST/auto-ascii-player-$MUSL_TARGET"

say "build: $WIN_TARGET (MinGW cross)"
cargo build --release --target "$WIN_TARGET" -p auto-ascii --features bin
cp "target/$WIN_TARGET/release/auto-ascii-player.exe" "$DIST/auto-ascii-player-$WIN_TARGET.exe"

say "build: auto-ascii-factory (native, informational)"
cargo build --release -p auto-ascii-factory
cp target/release/auto-ascii-factory "$DIST/auto-ascii-factory-$NATIVE_TARGET"

say "strip"
strip "$DIST/auto-ascii-player-$NATIVE_TARGET" "$DIST/auto-ascii-player-$MUSL_TARGET" \
      "$DIST/auto-ascii-factory-$NATIVE_TARGET"
x86_64-w64-mingw32-strip "$DIST/auto-ascii-player-$WIN_TARGET.exe"

say "artifacts"
ls -l "$DIST"

say "file(1)"
file "$DIST"/auto-ascii-player-* "$DIST"/auto-ascii-factory-*

say "ldd"
echo "-- native:"
ldd "$DIST/auto-ascii-player-$NATIVE_TARGET" || true
echo "-- musl (must be static):"
MUSL_LDD=$(ldd "$DIST/auto-ascii-player-$MUSL_TARGET" 2>&1 || true)
echo "$MUSL_LDD"
case "$MUSL_LDD" in
    *"not a dynamic executable"*|*"statically linked"*) ;;
    *) echo "FAIL: musl player is not fully static"; exit 1 ;;
esac
echo "-- windows: (PE binary; ldd not applicable)"

say "size gate: player < 5 MB each"
fail=0
for f in "$DIST/auto-ascii-player-$NATIVE_TARGET" \
         "$DIST/auto-ascii-player-$MUSL_TARGET" \
         "$DIST/auto-ascii-player-$WIN_TARGET.exe"; do
    sz=$(stat -c%s "$f")
    printf '%-55s %8d bytes (%d KiB)\n' "$(basename "$f")" "$sz" $((sz / 1024))
    if [ "$sz" -ge "$MAX_PLAYER_BYTES" ]; then
        echo "FAIL: $f >= 5 MB"
        fail=1
    fi
done
sz=$(stat -c%s "$DIST/auto-ascii-factory-$NATIVE_TARGET")
printf '%-55s %8d bytes (%d KiB) [informational, not gated]\n' \
    "$(basename "$DIST/auto-ascii-factory-$NATIVE_TARGET")" "$sz" $((sz / 1024))
[ "$fail" = 0 ] || exit 1

say "windows smoke test"
if command -v wine >/dev/null 2>&1; then
    if timeout 60 wine "$DIST/auto-ascii-player-$WIN_TARGET.exe" --version; then
        echo "wine smoke: OK"
    else
        echo "wine smoke: FAILED (cross binary still shipped; investigate)"
    fi
else
    echo "wine not installed → $WIN_TARGET ships as UNTESTED-CROSS (documented)."
fi

say "release.sh: all gates green"
