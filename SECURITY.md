# Security

## Reporting a vulnerability

Report vulnerabilities privately through GitHub's private vulnerability
reporting: <https://github.com/andmckay01/auto-ascii/security/advisories/new>.
Do not open a public issue for a security problem. You will get an
acknowledgement within a week; fixes ship as a new release.

## Supported versions

Only the latest release on
<https://github.com/andmckay01/auto-ascii/releases> receives fixes.

## What is in scope

- The `auto-ascii` binary and the published crates.
- The installers `install.sh` and `install.ps1` attached to each release.
- The release pipeline (`.github/workflows/`), the npm launcher package and
  the `@auto-ascii/*` platform packages, and the Homebrew tap formula.

## Verifying a download

Every release archive has a `.sha256` beside it and `SHA256SUMS` lists them
all; both installers check the archive against its `.sha256` before
installing. Every release after 0.3.0 also carries GitHub build provenance
attestations for its archives and Homebrew bottles, which tie each file to
the release workflow run that built it; v0.3.0 has none. `--signer-workflow`
makes the check fail unless `release.yml` signed the attestation:

```sh
gh attestation verify <file> --repo andmckay01/auto-ascii --signer-workflow andmckay01/auto-ascii/.github/workflows/release.yml
```

npm packages publish with provenance, shown on each package's npm page.
