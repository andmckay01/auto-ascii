#!/usr/bin/env node
// Launcher for the auto-ascii CLI. npm installs exactly one @auto-ascii/<platform>
// package next to this one (optionalDependencies filtered by os/cpu); this finds
// that package's prebuilt binary and runs it with our arguments.
'use strict';

const { spawn } = require('child_process');
const path = require('path');

// Mirrors the table in npm/scripts/targets.mjs; keep the two in sync.
const PACKAGES = {
  'darwin-arm64': '@auto-ascii/darwin-arm64',
  'darwin-x64': '@auto-ascii/darwin-x64',
  'linux-x64': '@auto-ascii/linux-x64',
  'linux-arm64': '@auto-ascii/linux-arm64',
  'win32-x64': '@auto-ascii/win32-x64',
  'win32-arm64': '@auto-ascii/win32-arm64',
};

const platform = `${process.platform}-${process.arch}`;

function fail(problem) {
  process.stderr.write(
    `auto-ascii: ${problem} (detected ${platform}).\n` +
      'Re-run `npm install -g auto-ascii` without --omit=optional/--no-optional, or install with ' +
      '`curl -fsSL https://github.com/andmckay01/auto-ascii/releases/latest/download/install.sh | sh`, ' +
      '`brew install andmckay01/tap/auto-ascii` or `cargo install auto-ascii`.\n',
  );
  process.exit(1);
}

function binaryPath() {
  const pkg = PACKAGES[platform];
  if (!pkg) fail('no prebuilt binary is published for this platform');
  let dir;
  try {
    dir = path.dirname(require.resolve(`${pkg}/package.json`));
  } catch {
    fail(`the platform package ${pkg} is not installed`);
  }
  const exe = process.platform === 'win32' ? 'auto-ascii.exe' : 'auto-ascii';
  return path.join(dir, 'bin', exe);
}

const bin = binaryPath();
const child = spawn(bin, process.argv.slice(2), { stdio: 'inherit' });

// Pass termination signals on, so a signal sent to this process alone still
// lets the binary restore the terminal instead of leaving it orphaned.
for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
  process.on(signal, () => child.kill(signal));
}

child.on('error', (err) => {
  process.stderr.write(`auto-ascii: could not run ${bin}: ${err.message}\n`);
  process.exit(1);
});
child.on('exit', (code, signal) => {
  if (signal) {
    // Die of the same signal so the shell sees what the child saw. If the
    // signal does not terminate us (Node ignores some), fall through to exit 1.
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
  }
  process.exit(code ?? 1);
});
