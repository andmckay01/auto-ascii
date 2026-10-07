# Releasing

One pushed tag releases everything. `.github/workflows/release.yml` runs the
same checks as CI, builds the `auto-ascii` binary natively for six targets,
publishes a GitHub release with archives, checksums, provenance attestations
and install scripts, then publishes to crates.io, npm and the Homebrew tap.
Every publish step skips what is already published, and the `release` job
carries on when the published `SHA256SUMS` matches its own, so re-running a
failed job, attestation or publish, is safe. "Re-run all jobs" after the
release is published rebuilds the binaries, and the rebuild's checksums
differ, so it fails on purpose: a published release is never replaced.

The shared macOS test jobs disable `kern.timer.coalescing_enabled` inside
the disposable CI guests, verify it is disabled, then restore the original
value after the tests even when they fail. The default hosted policy made
50 ms waits commonly take 100–125 ms, which broke real-time playback and
split terminal-reply fixtures. Disabling coalescing restored roughly
50.1 ms waits on both ARM and Intel; every existing assertion stays gated.
This is a CI policy, not a change to application scheduling. Apple's
[XNU registration](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sysctl.c#L3053)
and [timer implementation](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/kern/timer_call.c#L1800)
describe the writable switch and its effect on timer leeway.

## Cutting a release

1. Bump `[workspace.package] version` in the root `Cargo.toml` and all
   internal crate dependency version requirements in `[workspace.dependencies]`
   and `crates/auto-ascii/Cargo.toml`. Run `cargo update --workspace` to refresh
   `Cargo.lock` without updating external dependencies. Every published crate inherits
   the workspace version; `npm/auto-ascii/package.json` is stamped by the
   workflow, so leave it alone.
2. Move the `CHANGELOG.md` entry from "unreleased" to the version.
3. Commit on a branch and merge it through a PR. Once it has merged, tag
   the merge commit on `main` and push the tag:

   ```bash
   git checkout main && git pull && git tag vX.Y.Z && git push origin vX.Y.Z
   ```

The `version` job fails the run if the tag is not `v` plus the Cargo version,
or if the tagged commit is not on `main`'s first-parent line. Every commit of
a merged PR is an ancestor of `main`, but only its merge commit is on that
line, so tag `main` after the merge, as above. When the check fails, the tag
already exists locally and on GitHub, so delete it in both places before
tagging `main` again:

```bash
git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z
git checkout main && git pull && git tag vX.Y.Z && git push origin vX.Y.Z
```

A tag with a `-` (`v0.4.0-rc.1`) is a prerelease: a GitHub prerelease, which
the install scripts' default of "latest" ignores, and the `next` dist-tag on
npm. crates.io takes it like any other version, and `publish-homebrew` is
skipped, so the tap formula stays on the last stable release.

## What the workflow does

| job | runs on | does |
|---|---|---|
| `version` | every run | reads the Cargo workspace version; on a tag, checks the tag matches and that the tagged commit is on `main`'s first-parent line |
| `checks` | every run | `.github/workflows/checks.yml`, the same lint, test, cargo-deny, `publish-dry-run` and pipeline jobs CI runs (rustfmt is not gated), run again on the tagged commit before anything publishes. Linux and macOS tests run in release mode with one test thread, with a debug factory unit pass retaining its debug-only precondition tests. macOS tests run on macOS 26 ARM and macOS 15 Intel; release builds retain macOS 15. Every platform gates the release. The installers job is off here: it would install the previous release, and `smoke` tests the new installers against the new release instead |
| `publish-dry-run` | every run, in `checks` | `DRY_RUN=1 scripts/publish-crates.sh`: packages and verifies every crate, resolving each against the others' local packages, and uploads nothing |
| `build` | every run | `.github/workflows/build.yml`, shared with CI: builds, runs `--version`, packages each target (below); cold here, with no cache, so a poisoned or stale cache can never reach a shipped binary |
| `release` | tags only | repacks the macOS and Linux archives as Homebrew bottles (`scripts/homebrew-bottles.sh`), creates the GitHub release with every asset attached (gh drafts, uploads, then publishes), then attests build provenance for every archive and bottle in `SHA256SUMS`. A draft left by a failed run is deleted and recreated. A release that is already published passes when its `SHA256SUMS` matches this run's, as on a re-run of the failed job, and fails the job on purpose when it differs, as after a rebuild |
| `publish-crates` | tags only | `scripts/publish-crates.sh`: `cargo publish` for `auto-ascii-format`, `-core`, `-term`, `-eval`, `-factory`, then `auto-ascii` |
| `publish-npm` | tags only | stamps the version, generates the six `@auto-ascii/*` platform packages, publishes them, then `auto-ascii` |
| `publish-homebrew` | stable tags only | renders `Formula/auto-ascii.rb`, bottle block included, with `scripts/homebrew-formula.sh` and pushes it to `andmckay01/homebrew-tap` |
| `smoke` | tags only | installs the release just published with `install.sh` on Linux and macOS and `install.ps1` on Windows, pinned to this version, and checks `--version` reports it |
| `assemble` | CI only | rehearses the `release` job's assembly from CI's build on every PR, push to `main` and weekly run: the bottles, `sha256sum -c` over every `.sha256`, the Homebrew formula (`ruby -c`) and `npm pack --dry-run` of every npm package |

| target | runner | archive |
|---|---|---|
| `aarch64-apple-darwin` | `macos-15` | `.tar.gz` |
| `x86_64-apple-darwin` | `macos-15-intel` | `.tar.gz` |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` | `.tar.gz` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `windows-2022` | `.zip` |
| `aarch64-pc-windows-msvc` | `windows-11-arm` | `.zip` |

The Linux builds are linked on Ubuntu 22.04, so they need glibc 2.35 or later,
and they need `libasound2` (the ALSA library, `libasound.so.2`) at runtime.
No musl build is published. The Windows builds link the C runtime statically
(`+crt-static`), so they don't need the Visual C++ runtime installed.

### Names

- Release assets: `auto-ascii-<target>.tar.gz` or `.zip`, holding
  `auto-ascii[.exe]`, `LICENSE` and `README.md` at the top level; a
  `.sha256` beside each (`<hex>  <file>`); `SHA256SUMS` with all of them;
  `install.sh`; `install.ps1`.
- Homebrew bottles: `auto-ascii-<version>.<tag>.bottle.tar.gz` and its
  `.sha256`, one per macOS and Linux archive, holding the same binary at
  `auto-ascii/<version>/bin/auto-ascii`. The formula's `root_url` is the
  release download URL. Without a bottle, brew treats the formula as a
  source build and refuses to run on a Mac whose Xcode or Command Line
  Tools are out of date, even though nothing is compiled. The macOS tags are
  `arm64_big_sur` and `big_sur` because Rust's default deployment target is
  macOS 11 on arm64 and older on Intel; brew pours an older macOS tag on any
  newer macOS.
- Workflow artifacts, kept 7 days from every CI run (PRs, `main`, the weekly
  run) and 30 days from every release run, manual ones included:
  `archive-<target>` (the archive and its `.sha256`, from
  `scripts/package-archive.sh`) and `bin-<target>` (the stripped binary at
  `<target>/auto-ascii[.exe]`). Tag runs also keep
  `homebrew-bottle-sha256`, the bottles' `.sha256` files, which
  `publish-homebrew` reads.

## How users install

```bash
curl -fsSL https://github.com/andmckay01/auto-ascii/releases/latest/download/install.sh | sh
powershell -c "irm https://github.com/andmckay01/auto-ascii/releases/latest/download/install.ps1 | iex"
brew install andmckay01/tap/auto-ascii
npm i -g auto-ascii        # or: npx auto-ascii
cargo install auto-ascii   # or: cargo binstall auto-ascii
```

Both install scripts verify the archive's SHA-256 before installing, and
neither needs root or admin. In both, `AUTO_ASCII_VERSION` pins a release
and `AUTO_ASCII_INSTALL_DIR` moves the install from `~/.local/bin`
(`%LOCALAPPDATA%\Programs\auto-ascii` on Windows); PowerShell also takes
`-Version` and `-InstallDir`.

Every archive and bottle of each release after 0.3.0 also carries a build
provenance attestation; v0.3.0 has none. This checks that a file was built
by this repository's release workflow:

```bash
gh attestation verify <file> --repo andmckay01/auto-ascii --signer-workflow andmckay01/auto-ascii/.github/workflows/release.yml
```

## Secrets and registry setup

The workflow holds no registry tokens: crates.io and npm publish through
trusted publishing (OIDC) only. Both registries let you configure trusted
publishing only for a package that already exists, so a brand-new crate or
npm package is published once by hand from a maintainer machine, then given
trusted publishing before the next tag. A new crate needs a crates.io token
with the `publish-new` scope. A new `@auto-ascii/*` package needs your npm
account to own the `@auto-ascii` scope (an npm organization named
`auto-ascii`); build it from a run's artifacts as under "Testing without
releasing". Nothing reads a `CARGO_REGISTRY_TOKEN` or `NPM_TOKEN` secret, so
delete any left from the first release.

Trusted publishing is set up in three places:

- **GitHub**: the three publish jobs run in the `release` environment
  (repository Settings → Environments), whose deployment rule allows only
  the tag pattern `v*`. A publish token can therefore only be minted by a
  tag push, never by a branch or a manual run.
- **crates.io**: for each of the six crates, Settings → Trusted Publishing →
  add GitHub, owner `andmckay01`, repository `auto-ascii`, workflow
  `release.yml`, environment `release`.
- **npm**: for each of the seven packages (`auto-ascii` and the six
  `@auto-ascii/*`), package Settings → Trusted publishing → GitHub Actions,
  owner `andmckay01`, repository `auto-ascii`, workflow `release.yml`,
  environment `release`. Tick plain `npm publish`, not only
  `npm stage publish`.

`rust-lang/crates-io-auth-action` exchanges the job's OIDC token for a
short-lived crates.io token; npm 11.5.1 or later does the exchange itself, so
the job installs a pinned npm 11. Trusted publishing also attaches npm
provenance to every package, so the workflow passes no `--provenance` flag.

One secret remains:

- `HOMEBREW_TAP_TOKEN`: a fine-grained personal access token with access to
  `andmckay01/homebrew-tap` only, permission Contents: read and write. Keep
  it in the `release` environment (Settings → Environments → `release` →
  Environment secrets) rather than as a repository secret, so the `v*` rule
  gates it too; `secrets.HOMEBREW_TAP_TOKEN` resolves from either scope.
  Without it `publish-homebrew` prints a warning and succeeds, and the rest
  of the release is unaffected.

## Testing without releasing

Only tag pushes publish. CI already builds and packages all six targets on
every PR, push to `main` and weekly run, through the same
`.github/workflows/build.yml`, and its `assemble` job rehearses the
release's bottles, formula and npm packages from that build, so
`release.yml` has no `pull_request` trigger. To run the release workflow's
checks, build and packaging on a branch by hand:

```bash
gh workflow run release.yml --ref <branch>
gh run watch
```

`workflow_dispatch` works once `release.yml` is on `main`.

To build the npm packages locally from a CI or release run's `bin-*`
artifacts (gh puts each artifact in its own directory; the copy merges them
into `artifacts/<target>/`):

```bash
dl=$(mktemp -d)
gh run download <run-id> --pattern 'bin-*' --dir "$dl"
mkdir -p artifacts && cp -R "$dl"/bin-*/* artifacts/
node npm/scripts/set-version.mjs X.Y.Z
node npm/scripts/make-platform-packages.mjs --version X.Y.Z --artifacts artifacts --out npm-dist
git checkout -- npm/auto-ascii/package.json   # undo the version stamp
```

`npm pack ./npm-dist/<platform>` then shows exactly what would be published.
`DRY_RUN=1 bash scripts/publish-crates.sh` does the same for the crates.

## Bumping the toolchain

CI, the release build and local `cargo` all use the toolchain pinned in
`rust-toolchain.toml`, so a new Rust release never changes a build on its
own. To move to one, edit `channel` in `rust-toolchain.toml`, run `make lint`
and `make test` (rustup installs the new toolchain on first use), fix what
they flag, and commit.
