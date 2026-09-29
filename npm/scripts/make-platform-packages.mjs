#!/usr/bin/env node
// Usage: node npm/scripts/make-platform-packages.mjs --version <v> --artifacts <dir> --out <dir>
//
// Turns release binaries laid out as <artifacts>/<target-triple>/auto-ascii
// (auto-ascii.exe on Windows) into one publishable npm package per platform,
// at <out>/<platform>/ (e.g. <out>/darwin-arm64/). Run set-version.mjs first:
// the version must match the pins in npm/auto-ascii/package.json.

import { chmodSync, copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { PLATFORMS, binaryName, normalizeVersion, packageDir, platformForTarget } from './targets.mjs';

const mainDir = fileURLToPath(new URL('../auto-ascii/', import.meta.url));

function main() {
  const { values } = parseArgs({
    options: { version: { type: 'string' }, artifacts: { type: 'string' }, out: { type: 'string' } },
  });
  if (!values.version || !values.artifacts || !values.out) {
    throw new Error('usage: make-platform-packages.mjs --version <v> --artifacts <dir> --out <dir>');
  }
  const version = normalizeVersion(values.version);
  const artifacts = resolve(values.artifacts);
  const out = resolve(values.out);

  const launcher = JSON.parse(readFileSync(join(mainDir, 'package.json'), 'utf8'));
  if (launcher.version !== version) {
    throw new Error(`--version ${version} does not match auto-ascii/package.json ${launcher.version}; run set-version.mjs first`);
  }

  // Check every target dir before writing anything.
  const errors = [];
  const builds = new Map(); // platform -> { triple, binary }
  for (const entry of readdirSync(artifacts, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const triple = entry.name;
    const platform = platformForTarget(triple);
    if (!platform) {
      errors.push(`unknown target dir ${triple} (known: ${PLATFORMS.flatMap((p) => p.targets).join(', ')})`);
      continue;
    }
    const binary = join(artifacts, triple, binaryName(platform));
    if (!existsSync(binary)) {
      errors.push(`${triple} has no ${binaryName(platform)}`);
    } else if (builds.has(platform)) {
      errors.push(`${builds.get(platform).triple} and ${triple} both map to ${platform.name}; provide one`);
    } else {
      builds.set(platform, { triple, binary });
    }
  }
  if (errors.length) throw new Error(errors.join('\n  '));
  for (const platform of PLATFORMS) {
    if (!builds.has(platform)) {
      console.warn(`warning: no artifacts for ${platform.name} (looked for ${platform.targets.join(', ')})`);
    }
  }
  if (!builds.size) throw new Error(`no target dirs in ${artifacts}`);

  const generated = [];
  for (const [platform, { binary }] of builds) {
    const dir = join(out, packageDir(platform));
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(join(dir, 'bin'), { recursive: true });

    const manifest = {
      name: platform.name,
      version,
      description: `auto-ascii prebuilt binary for ${platform.os}/${platform.cpu}`,
      license: launcher.license,
      homepage: launcher.homepage,
      repository: { type: launcher.repository.type, url: launcher.repository.url },
      os: [platform.os],
      cpu: [platform.cpu],
      files: ['bin'],
      publishConfig: { access: 'public' },
    };
    writeFileSync(join(dir, 'package.json'), JSON.stringify(manifest, null, 2) + '\n');

    const target = join(dir, 'bin', binaryName(platform));
    copyFileSync(binary, target);
    chmodSync(target, 0o755);
    copyFileSync(join(mainDir, 'LICENSE'), join(dir, 'LICENSE'));
    writeFileSync(
      join(dir, 'README.md'),
      `# ${platform.name}\n\n` +
        `The prebuilt \`auto-ascii\` binary for ${platform.os}/${platform.cpu}.\n\n` +
        'Install [`auto-ascii`](https://www.npmjs.com/package/auto-ascii) instead; it depends on this package and runs the binary.\n',
    );
    generated.push(dir);
  }
  console.log(`generated ${generated.length} package(s) at ${version}:`);
  for (const dir of generated) console.log(dir);
}

try {
  main();
} catch (err) {
  console.error(`make-platform-packages: ${err.message}`);
  process.exit(1);
}
