# Releasing

One pushed tag releases everything. `.github/workflows/release.yml` builds
the `auto-ascii` binary natively for six targets, publishes a GitHub release
with archives, checksums and install scripts, then publishes to crates.io,
npm and the Homebrew tap. Every publish step skips what is already published,
so re-running a failed job is always safe.

## Cutting a release

1. Bump `[workspace.package] version` in the root `Cargo.toml`. Every
   published crate inherits it; `npm/auto-ascii/package.json` is stamped by
   the workflow, so leave it alone.
2. Move the `CHANGELOG.md` entry from "unreleased" to the version.
3. Commit, then tag and push the tag:

   ```bash
   git tag vX.Y.Z && git push origin vX.Y.Z
   ```

The `version` job fails the run if the tag is not `v` plus the Cargo version.
A tag with a `-` (`v0.4.0-rc.1`) is a prerelease: a GitHub prerelease, which
the install scripts' default of "latest" ignores, and the `next` dist-tag on
npm. crates.io takes it like any other version, and `publish-homebrew` is
skipped, so the tap formula stays on the last stable release.

## What the workflow does

| job | runs on | does |
|---|---|---|
| `version` | every run | reads the Cargo workspace version; on a tag, checks the tag matches |
| `build` | every run | builds, runs `--version`, packages each target (below) |
| `release` | tags only | creates the GitHub release with every asset attached (gh drafts, uploads, then publishes); if it already exists, re-uploads the assets with `--clobber` |
| `publish-crates` | tags only | `scripts/publish-crates.sh`: `cargo publish` for `auto-ascii-format`, `-core`, `-term`, `-eval`, `-factory`, then `auto-ascii` |
| `publish-npm` | tags only | stamps the version, generates the six `@auto-ascii/*` platform packages, publishes them, then `auto-ascii` |
| `publish-homebrew` | stable tags only | renders `Formula/auto-ascii.rb` with `scripts/homebrew-formula.sh` and pushes it to `andmckay01/homebrew-tap` |

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
- Workflow artifacts, kept for every run including PRs and manual runs:
  `archive-<target>` (the archive and its `.sha256`, from
  `scripts/package-archive.sh`) and `bin-<target>` (the stripped binary at
  `<target>/auto-ascii[.exe]`).

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

## Secrets and registry setup

Repository secrets (Settings → Secrets and variables → Actions):

- `HOMEBREW_TAP_TOKEN`: a fine-grained personal access token with access to
  `andmckay01/homebrew-tap` only, permission Contents: read and write.
  Without it `publish-homebrew` prints a warning and succeeds, and the rest
  of the release is unaffected.
- `CARGO_REGISTRY_TOKEN` and `NPM_TOKEN`: for the **first** release only.
  Both registries let you configure trusted publishing only for a package
  that already exists, and the six `@auto-ascii/*` npm packages and any
  crate not yet on crates.io don't. The crates.io token needs the
  `publish-new` and `publish-update` scopes. The npm token must be able to
  publish new packages without an OTP, and your npm account must own the
  `@auto-ascii` scope (an npm organization named `auto-ascii`).

After the first release, switch both registries to trusted publishing (OIDC)
and delete the two tokens, from the repository secrets and from the
registries:

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

With a token secret set the jobs use it; with none they fall back to OIDC
(`rust-lang/crates-io-auth-action` for crates.io; npm 11.5.1+ for npm).

## Testing without releasing

Only tag pushes publish. To run the build and packaging on a branch:

```bash
gh workflow run release.yml --ref <branch>
gh run watch
```

`workflow_dispatch` works once `release.yml` is on `main`. Before that, and
for any PR that touches `release.yml`, `scripts/` or `npm/`, the
`pull_request` trigger runs the same `version` and `build` jobs.

To build the npm packages locally from a run's `bin-*` artifacts (gh puts
each artifact in its own directory; the copy merges them into
`artifacts/<target>/`):

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
