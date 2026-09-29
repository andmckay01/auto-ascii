#!/bin/sh
# Installs the auto-ascii command on macOS or Linux from a GitHub release:
#   curl -fsSL https://github.com/andmckay01/auto-ascii/releases/latest/download/install.sh | sh
# AUTO_ASCII_VERSION=0.3.0 installs that release instead of the latest, and
# AUTO_ASCII_INSTALL_DIR=<dir> installs somewhere other than ~/.local/bin.
# Every download is checked against its published SHA-256; nothing needs sudo.
set -eu

REPO=andmckay01/auto-ascii

err() {
    printf 'auto-ascii install: %s\n' "$*" >&2
    exit 1
}

os=$(uname -s)
case "$os" in
    Darwin) vendor_os=apple-darwin ;;
    Linux) vendor_os=unknown-linux-gnu ;;
    *) err "unsupported OS $os (on Windows use install.ps1; elsewhere: cargo install auto-ascii)" ;;
esac

machine=$(uname -m)
case "$machine" in
    x86_64 | amd64) arch=x86_64 ;;
    arm64 | aarch64) arch=aarch64 ;;
    *) err "unsupported architecture $machine (try: cargo install auto-ascii)" ;;
esac

if [ "$os" = Linux ] && ldd --version 2>&1 | grep -qi musl; then
    err "this looks like a musl system (e.g. Alpine), and no musl build is published; try: cargo install auto-ascii"
fi

target="$arch-$vendor_os"
VERSION="${AUTO_ASCII_VERSION:-latest}"
if [ "$VERSION" = latest ]; then
    base="https://github.com/$REPO/releases/latest/download"
else
    base="https://github.com/$REPO/releases/download/v${VERSION#v}"
fi
INSTALL_DIR="${AUTO_ASCII_INSTALL_DIR:-$HOME/.local/bin}"
archive="auto-ascii-$target.tar.gz"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO- "$1" >"$2"; }
else
    err "need curl or wget to download the release"
fi

if command -v sha256sum >/dev/null 2>&1; then
    check_sum() { sha256sum -c "$1"; }
elif command -v shasum >/dev/null 2>&1; then
    check_sum() { shasum -a 256 -c "$1"; }
else
    err "need sha256sum or shasum to verify the download; refusing to install unverified"
fi

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t auto-ascii)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' HUP INT TERM

printf 'downloading %s (%s)\n' "$archive" "$VERSION"
fetch "$base/$archive" "$tmp/$archive" ||
    err "could not download $base/$archive (is $VERSION a published release?)"
fetch "$base/$archive.sha256" "$tmp/$archive.sha256" ||
    err "could not download $base/$archive.sha256"

if ! (cd "$tmp" && check_sum "$archive.sha256") >/dev/null 2>&1; then
    err "SHA-256 mismatch for $archive; refusing to install"
fi

tar -xzf "$tmp/$archive" -C "$tmp" auto-ascii
mkdir -p "$INSTALL_DIR"
staged_for_atomic_rename="$INSTALL_DIR/.auto-ascii.new"
cp "$tmp/auto-ascii" "$staged_for_atomic_rename"
chmod 755 "$staged_for_atomic_rename"
mv -f "$staged_for_atomic_rename" "$INSTALL_DIR/auto-ascii"

bin="$INSTALL_DIR/auto-ascii"
if ! installed=$("$bin" --version); then
    printf 'auto-ascii install: copied to %s, but it does not run here.\n' "$bin" >&2
    if [ "$os" = Linux ]; then
        ldd "$bin" 2>/dev/null | grep 'not found' >&2 || true
        printf '%s\n' \
            "It links the ALSA library libasound.so.2: install libasound2 (Debian/Ubuntu: sudo apt install libasound2t64, or libasound2 on older releases; Fedora: sudo dnf install alsa-lib)." \
            "Otherwise it needs glibc 2.35 or later." >&2
    fi
    err "or build it from source: cargo install auto-ascii"
fi
printf 'installed %s to %s\n' "$installed" "$bin"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        case "${SHELL:-}" in
            */zsh) rc="$HOME/.zshrc" ;;
            */bash) if [ "$os" = Darwin ]; then rc="$HOME/.bash_profile"; else rc="$HOME/.bashrc"; fi ;;
            *) rc="$HOME/.profile" ;;
        esac
        case "$INSTALL_DIR" in
            "$HOME"/*) shown="\$HOME/${INSTALL_DIR#"$HOME"/}" ;;
            *) shown="$INSTALL_DIR" ;;
        esac
        printf '\n%s is not on your PATH. Add this line to %s, then open a new terminal:\n\n' "$INSTALL_DIR" "$rc"
        printf '    export PATH="%s:%s"\n' "$shown" "\$PATH"
        ;;
esac

cat <<'QUICKSTART'

Get started:

    auto-ascii add "https://youtu.be/jNQXAC9IVRw"   # import a YouTube link into your library
    auto-ascii play me-at-the-zoo                   # q quits
    auto-ascii stream me at the zoo                 # play the first search result live, saving nothing
QUICKSTART
