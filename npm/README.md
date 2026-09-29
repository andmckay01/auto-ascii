# npm distribution (maintainer notes)

The `auto-ascii` command ships on npm the way esbuild and Biome ship theirs:
a main package whose only code is a launcher, plus one package per platform
holding the prebuilt binary. The main package lists every platform package in
`optionalDependencies`, and each platform package declares `os` and `cpu`, so
npm installs exactly the one that matches. There is no postinstall script and
no install-time download. Every package publishes at the same version, equal
to the release tag.

```
npm/
  auto-ascii/            the main package, committed and published as-is
    bin/auto-ascii.js    finds @auto-ascii/<os>-<cpu> and runs its binary
  scripts/
    targets.mjs          Rust target triple -> platform package table
    set-version.mjs      stamps a version into auto-ascii/package.json
    make-platform-packages.mjs  generates the platform packages from binaries
    smoke.sh             end-to-end local check with a fake binary
```

`scripts/targets.mjs` is the single source of truth for which targets ship.
The launcher keeps its own copy of the `os`/`cpu` -> package part, and
`set-version.mjs` fails if `optionalDependencies` drifts from the table.
Linux accepts either the `-gnu` or the `-musl` triple for a CPU, not both.

## Release flow

The release workflow builds one binary per target and lays them out as

```
<artifacts>/<target-triple>/auto-ascii        # auto-ascii.exe on Windows
```

for example `artifacts/aarch64-apple-darwin/auto-ascii`. It then runs, from
the repository root:

```bash
node npm/scripts/set-version.mjs "$TAG"      # v0.3.0 or 0.3.0
node npm/scripts/make-platform-packages.mjs --version "$TAG" --artifacts artifacts --out npm-dist
# publish every npm-dist/<platform>/ first, then npm/auto-ascii
```

`make-platform-packages.mjs` refuses a version that differs from
`auto-ascii/package.json` (run `set-version.mjs` first), fails on an unknown
target directory or a target directory with no binary, and warns about
platforms with no artifacts. Publish the platform packages before the main
package, so its pins resolve the moment it is live.

## Checking it locally

```bash
bash npm/scripts/smoke.sh
```

Needs Node 18+ and npm, macOS or Linux, and no network. It packs and installs
the main package plus a platform package for this machine whose binary is a
shell script, runs `npx auto-ascii`, checks the arguments and exit code pass
through, and restores `auto-ascii/package.json`. Temporary files go under
`$TMPDIR` and are removed on exit.
