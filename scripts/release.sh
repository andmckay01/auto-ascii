#!/usr/bin/env bash
# scripts/release.sh — M5 item E: stripped release player binaries.
#
# Targets:
#   x86_64-unknown-linux-gnu   native (host toolchain)
#   x86_64-unknown-linux-musl  fully static (rustup target + apt musl-tools);
#                              verified with `ldd` → "not a dynamic executable"
#   x86_64-pc-windows-gnu      MinGW cross build (apt mingw-w64). Compiles and
#                              links here; smoke-tested through wine ONLY when
#                              wine is already installed, otherwise it ships as
#                              UNTESTED-CROSS (headless Linux box — documented
#                              in README "Install").
#   macOS: no cross build (no osxcross by policy) — build on a Mac:
#          `cargo build --release -p sleepytime --features bin` (see Makefile).
#
# Output: dist/sleepy-player-<target>[.exe], stripped, each REQUIRED < 5 MB
# (PLAN §7 M5 "binaries <5 MB"). The factory is built natively and reported
# too (informational — it may be bigger; only the player is gated).
#
# Prerequisites are installed on demand (idempotent): rustup targets via
# `rustup target add`, C toolchains via `sudo -n apt-get install` (musl-tools,
# mingw-w64). Pass NO_APT=1 to forbid apt (fails if a toolchain is missing).

set -euo pipefail
cd "$(dirname "$0")/.."

DIST=dist
MAX_PLAYER_BYTES=$((5 * 1024 * 1024))
MUSL_TARGET=x86_64-unknown-linux-musl
WIN_TARGET=x86_64-pc-windows-gnu
NATIVE_TARGET=$(rustc -vV | sed -n 's/^host: //p')

say() { printf '\n== %s\n' "$*"; }

# --- prerequisites ----------------------------------------------------------
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

# --- builds -----------------------------------------------------------------
say "build: native ($NATIVE_TARGET)"
cargo build --release -p sleepytime --features bin
cp target/release/sleepy-player "$DIST/sleepy-player-$NATIVE_TARGET"

say "build: $MUSL_TARGET (fully static)"
cargo build --release --target "$MUSL_TARGET" -p sleepytime --features bin
cp "target/$MUSL_TARGET/release/sleepy-player" "$DIST/sleepy-player-$MUSL_TARGET"

say "build: $WIN_TARGET (MinGW cross)"
cargo build --release --target "$WIN_TARGET" -p sleepytime --features bin
cp "target/$WIN_TARGET/release/sleepy-player.exe" "$DIST/sleepy-player-$WIN_TARGET.exe"

say "build: sleepy-factory (native, informational)"
cargo build --release -p sleepy-factory
cp target/release/sleepy-factory "$DIST/sleepy-factory-$NATIVE_TARGET"

# --- strip ------------------------------------------------------------------
say "strip"
strip "$DIST/sleepy-player-$NATIVE_TARGET" "$DIST/sleepy-player-$MUSL_TARGET" \
      "$DIST/sleepy-factory-$NATIVE_TARGET"
x86_64-w64-mingw32-strip "$DIST/sleepy-player-$WIN_TARGET.exe"

# --- report + gates ---------------------------------------------------------
say "artifacts"
ls -l "$DIST"

say "file(1)"
file "$DIST"/sleepy-player-* "$DIST"/sleepy-factory-*

say "ldd"
echo "-- native:"
ldd "$DIST/sleepy-player-$NATIVE_TARGET" || true
echo "-- musl (must be static):"
MUSL_LDD=$(ldd "$DIST/sleepy-player-$MUSL_TARGET" 2>&1 || true)
echo "$MUSL_LDD"
case "$MUSL_LDD" in
    *"not a dynamic executable"*|*"statically linked"*) ;;
    *) echo "FAIL: musl player is not fully static"; exit 1 ;;
esac
echo "-- windows: (PE binary; ldd not applicable)"

say "size gate: player < 5 MB each"
fail=0
for f in "$DIST/sleepy-player-$NATIVE_TARGET" \
         "$DIST/sleepy-player-$MUSL_TARGET" \
         "$DIST/sleepy-player-$WIN_TARGET.exe"; do
    sz=$(stat -c%s "$f")
    printf '%-55s %8d bytes (%d KiB)\n' "$(basename "$f")" "$sz" $((sz / 1024))
    if [ "$sz" -ge "$MAX_PLAYER_BYTES" ]; then
        echo "FAIL: $f >= 5 MB"
        fail=1
    fi
done
sz=$(stat -c%s "$DIST/sleepy-factory-$NATIVE_TARGET")
printf '%-55s %8d bytes (%d KiB) [informational, not gated]\n' \
    "$(basename "$DIST/sleepy-factory-$NATIVE_TARGET")" "$sz" $((sz / 1024))
[ "$fail" = 0 ] || exit 1

# --- optional wine smoke ----------------------------------------------------
say "windows smoke test"
if command -v wine >/dev/null 2>&1; then
    if timeout 60 wine "$DIST/sleepy-player-$WIN_TARGET.exe" --version; then
        echo "wine smoke: OK"
    else
        echo "wine smoke: FAILED (cross binary still shipped; investigate)"
    fi
else
    echo "wine not installed → $WIN_TARGET ships as UNTESTED-CROSS (documented)."
fi

say "release.sh: all gates green"
