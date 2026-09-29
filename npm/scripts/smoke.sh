#!/usr/bin/env bash
# Offline end-to-end check of the npm distribution: wraps a fake binary (echoes
# its arguments, exits 7) in this machine's platform package, installs it with
# the main package into a throwaway project with its own npm cache, and runs
# the launcher through npx; npm tolerates the unresolvable optional platform
# packages. macOS and Linux only; auto-ascii/package.json is restored on exit.
set -euo pipefail
cd "$(dirname "$0")/.."

SMOKE_VERSION=0.0.0-smoke

say() { printf '\n== %s\n' "$*"; }
fail() { echo "smoke: FAIL: $*" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-/tmp}/auto-ascii-smoke.XXXXXX")
cp auto-ascii/package.json "$work/package.json.orig"
cleanup() {
    cp "$work/package.json.orig" auto-ascii/package.json
    rm -rf "$work"
}
trap cleanup EXIT
orig_version=$(node -p "require('./auto-ascii/package.json').version")

export npm_config_cache="$work/npm-cache" npm_config_offline=true
export npm_config_audit=false npm_config_fund=false npm_config_update_notifier=false

pack_tgz() {
    npm pack "$1" --pack-destination "$work" --json \
        | node -p "require('path').join(process.argv[1], JSON.parse(require('fs').readFileSync(0, 'utf8'))[0].filename)" "$work"
}

say "npm pack --dry-run auto-ascii"
files=$(npm pack --dry-run --json ./auto-ascii \
    | node -p "JSON.parse(require('fs').readFileSync(0, 'utf8'))[0].files.map((f) => f.path).sort().join(' ')")
echo "$files"
[ "$files" = "LICENSE README.md bin/auto-ascii.js package.json" ] || fail "unexpected files in auto-ascii: $files"

say "fake artifacts"
read -r triple pkg_dir < <(node --input-type=module -e "
import { PLATFORMS, packageDir } from './scripts/targets.mjs';
const p = PLATFORMS.find((p) => p.os === process.platform && p.cpu === process.arch);
if (p) console.log(p.targets[0], packageDir(p));") || true
[ -n "${triple:-}" ] || fail "no target in scripts/targets.mjs for this machine"
mkdir -p "$work/artifacts/$triple"
printf '#!/bin/sh\necho "$@"\nexit 7\n' > "$work/artifacts/$triple/auto-ascii"
echo "$triple -> $pkg_dir"

say "set-version + make-platform-packages ($SMOKE_VERSION)"
node scripts/set-version.mjs "$SMOKE_VERSION"
node scripts/make-platform-packages.mjs --version "$SMOKE_VERSION" --artifacts "$work/artifacts" --out "$work/packages"

say "pack + install"
platform_tgz=$(pack_tgz "$work/packages/$pkg_dir")
main_tgz=$(pack_tgz ./auto-ascii)
mkdir "$work/project"
(cd "$work/project" && npm init -y >/dev/null && npm install --loglevel=error "$platform_tgz" "$main_tgz")

say "npx auto-ascii hello --world"
status=0
out=$(cd "$work/project" && npx --no auto-ascii hello --world) || status=$?
echo "stdout: $out"
echo "exit: $status"
[[ "$out" == *"hello --world"* ]] || fail "binary did not receive the arguments"
[ "$status" = 7 ] || fail "expected exit 7, got $status"

say "missing platform package"
rm -rf "$work/project/node_modules/@auto-ascii/$pkg_dir"
status=0
err=$(cd "$work/project" && npx --no auto-ascii hello 2>&1 >/dev/null) || status=$?
echo "$err"
[ "$status" = 1 ] || fail "expected exit 1 without the platform package, got $status"
[[ "$err" == *"not installed"* ]] || fail "launcher did not explain the missing package"

say "restore $orig_version"
node scripts/set-version.mjs "$orig_version"
cmp -s auto-ascii/package.json "$work/package.json.orig" || fail "set-version round trip changed auto-ascii/package.json"

say "smoke passed"
