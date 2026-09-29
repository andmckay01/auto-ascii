// Rust target triple -> npm platform package: the single source of truth for
// the scripts in this directory. auto-ascii/bin/auto-ascii.js mirrors the
// os/cpu -> package part of it; keep the two in sync.

export const PLATFORMS = [
  { name: '@auto-ascii/darwin-arm64', os: 'darwin', cpu: 'arm64', targets: ['aarch64-apple-darwin'] },
  { name: '@auto-ascii/darwin-x64', os: 'darwin', cpu: 'x64', targets: ['x86_64-apple-darwin'] },
  {
    name: '@auto-ascii/linux-x64',
    os: 'linux',
    cpu: 'x64',
    targets: ['x86_64-unknown-linux-gnu', 'x86_64-unknown-linux-musl'],
  },
  {
    name: '@auto-ascii/linux-arm64',
    os: 'linux',
    cpu: 'arm64',
    targets: ['aarch64-unknown-linux-gnu', 'aarch64-unknown-linux-musl'],
  },
  { name: '@auto-ascii/win32-x64', os: 'win32', cpu: 'x64', targets: ['x86_64-pc-windows-msvc'] },
  { name: '@auto-ascii/win32-arm64', os: 'win32', cpu: 'arm64', targets: ['aarch64-pc-windows-msvc'] },
];

// The platform entry for a target triple, or undefined if we don't publish it.
export function platformForTarget(triple) {
  return PLATFORMS.find((p) => p.targets.includes(triple));
}

// The directory a generated platform package goes in: "darwin-arm64" for
// "@auto-ascii/darwin-arm64".
export function packageDir(platform) {
  return platform.name.split('/')[1];
}

export function binaryName(platform) {
  return platform.os === 'win32' ? 'auto-ascii.exe' : 'auto-ascii';
}

// "v1.2.3" or "1.2.3[-pre][+build]" -> "1.2.3[-pre][+build]"; throws otherwise.
export function normalizeVersion(raw) {
  const version = String(raw ?? '').replace(/^v/, '');
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error(`not a semver version: ${JSON.stringify(raw)}`);
  }
  return version;
}
