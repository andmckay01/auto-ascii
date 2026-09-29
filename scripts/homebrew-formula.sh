#!/usr/bin/env bash
# Usage: scripts/homebrew-formula.sh <version> <dist-dir>
# Prints the Homebrew formula for auto-ascii <version> to stdout, pinned to the
# release archives whose .sha256 files (from package-archive.sh) are in
# <dist-dir>. The release workflow commits it to andmckay01/homebrew-tap as
# Formula/auto-ascii.rb.
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <version> <dist-dir>" >&2
    exit 2
fi
version=${1#v}
dist=$2
url="https://github.com/andmckay01/auto-ascii/releases/download/v$version"

archive_sha256() {
    local file="$dist/auto-ascii-$1.tar.gz.sha256" hex
    [ -f "$file" ] || { echo "homebrew-formula: missing $file" >&2; return 1; }
    hex=$(cut -d' ' -f1 "$file")
    [[ "$hex" =~ ^[0-9a-f]{64}$ ]] || { echo "homebrew-formula: no SHA-256 in $file" >&2; return 1; }
    echo "$hex"
}

mac_arm=$(archive_sha256 aarch64-apple-darwin)
mac_intel=$(archive_sha256 x86_64-apple-darwin)
linux_arm=$(archive_sha256 aarch64-unknown-linux-gnu)
linux_intel=$(archive_sha256 x86_64-unknown-linux-gnu)

cat <<FORMULA
class AutoAscii < Formula
  desc "Realtime ASCII-art video in your terminal"
  homepage "https://github.com/andmckay01/auto-ascii"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$url/auto-ascii-aarch64-apple-darwin.tar.gz"
      sha256 "$mac_arm"
    end
    on_intel do
      url "$url/auto-ascii-x86_64-apple-darwin.tar.gz"
      sha256 "$mac_intel"
    end
  end

  on_linux do
    on_arm do
      url "$url/auto-ascii-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$linux_arm"
    end
    on_intel do
      url "$url/auto-ascii-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$linux_intel"
    end
  end

  def install
    bin.install "auto-ascii"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/auto-ascii --version")
  end
end
FORMULA
