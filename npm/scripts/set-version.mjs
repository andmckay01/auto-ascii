#!/usr/bin/env node
// Usage: node npm/scripts/set-version.mjs <version>
//
// Stamps <version> (a leading "v" is stripped, so a tag works) into
// npm/auto-ascii/package.json: the package's own version and every
// optionalDependencies pin, so the launcher only ever installs platform
// packages built from the same release.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { PLATFORMS, normalizeVersion } from './targets.mjs';

const manifestPath = fileURLToPath(new URL('../auto-ascii/package.json', import.meta.url));

function main(argv) {
  if (argv.length !== 1) throw new Error('usage: set-version.mjs <version>');
  const version = normalizeVersion(argv[0]);
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));

  // The pins must name exactly the packages make-platform-packages can build.
  const pinned = Object.keys(manifest.optionalDependencies ?? {}).sort();
  const expected = PLATFORMS.map((p) => p.name).sort();
  if (pinned.join() !== expected.join()) {
    throw new Error(`optionalDependencies ${pinned.join(', ')} do not match targets.mjs ${expected.join(', ')}`);
  }

  manifest.version = version;
  for (const name of pinned) manifest.optionalDependencies[name] = version;
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + '\n');
  console.log(`auto-ascii and ${pinned.length} platform pins set to ${version}`);
}

try {
  main(process.argv.slice(2));
} catch (err) {
  console.error(`set-version: ${err.message}`);
  process.exit(1);
}
