#!/usr/bin/env bash
# Usage: scripts/homebrew-formula.sh <version> <dist-dir>
# Prints the Homebrew formula for auto-ascii <version> to stdout, pinned to the
# release archives and bottles whose .sha256 files (package-archive.sh,
# homebrew-bottles.sh) are in <dist-dir>. The release workflow commits it to
# andmckay01/homebrew-tap as Formula/auto-ascii.rb.
set -euo pipefail

if [ $# -ne 2 ]; then
    echo "usage: $0 <version> <dist-dir>" >&2
    exit 2
fi
version=${1#v}
dist=$2
url="https://github.com/andmckay01/auto-ascii/releases/download/v$version"

sha256_of() {
    local file="$dist/$1.sha256" hex
    [ -f "$file" ] || { echo "homebrew-formula: missing $file" >&2; return 1; }
    hex=$(cut -d' ' -f1 "$file")
    [[ "$hex" =~ ^[0-9a-f]{64}$ ]] || { echo "homebrew-formula: no SHA-256 in $file" >&2; return 1; }
    echo "$hex"
}

mac_arm=$(sha256_of auto-ascii-aarch64-apple-darwin.tar.gz)
mac_intel=$(sha256_of auto-ascii-x86_64-apple-darwin.tar.gz)
linux_arm=$(sha256_of auto-ascii-aarch64-unknown-linux-gnu.tar.gz)
linux_intel=$(sha256_of auto-ascii-x86_64-unknown-linux-gnu.tar.gz)
bottle_mac_arm=$(sha256_of "auto-ascii-$version.arm64_big_sur.bottle.tar.gz")
bottle_mac_intel=$(sha256_of "auto-ascii-$version.big_sur.bottle.tar.gz")
bottle_linux_arm=$(sha256_of "auto-ascii-$version.arm64_linux.bottle.tar.gz")
bottle_linux_intel=$(sha256_of "auto-ascii-$version.x86_64_linux.bottle.tar.gz")

cat <<FORMULA
class AutoAscii < Formula
  desc "Realtime ASCII-art video in your terminal"
  homepage "https://github.com/andmckay01/auto-ascii"
  version "$version"
  license "MIT"

  bottle do
    root_url "$url"
    sha256 cellar: :any_skip_relocation, arm64_big_sur: "$bottle_mac_arm"
    sha256 cellar: :any_skip_relocation, big_sur:       "$bottle_mac_intel"
    sha256 cellar: :any_skip_relocation, arm64_linux:   "$bottle_linux_arm"
    sha256 cellar: :any_skip_relocation, x86_64_linux:  "$bottle_linux_intel"
  end

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
