/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  buildStrategyForTarget,
  platformPackageName,
  TARGETS,
  targetForSelector,
  targetsForHost,
} from '../scripts/platforms.js';

const linuxX64 = targetForSelector('linux-x64');
const linuxArm64 = targetForSelector('linux-arm64');
const darwinArm64 = targetForSelector('darwin-arm64');

const rootManifest = JSON.parse(
  readFileSync(new URL('../package.json', import.meta.url), 'utf8'),
);
assert.deepEqual(rootManifest.bin, { anolisa: 'bin/anolisa' });
for (const target of TARGETS) {
  const platformManifest = JSON.parse(
    readFileSync(
      new URL(
        `../platforms/${target.pkg_suffix}/package.json`,
        import.meta.url,
      ),
      'utf8',
    ),
  );
  assert.equal('bin' in platformManifest, false);
}

assert.equal(targetForSelector('linux-arm64').pkg_suffix, 'linux-arm64');
assert.equal(
  targetForSelector('aarch64-apple-darwin').pkg_suffix,
  'darwin-arm64',
);
assert.equal(targetForSelector('x64').pkg_suffix, 'linux-x64');
assert.throws(() => targetForSelector('arm64'), /ambiguous/);
assert.throws(() => targetForSelector('aarch64'), /ambiguous/);
assert.throws(() => targetForSelector('unknown'), /Unknown target/);

assert.equal(
  platformPackageName('darwin', 'arm64'),
  '@anolisa/cli-darwin-arm64',
);
assert.throws(
  () => platformPackageName('darwin', 'x64'),
  /No prebuilt binary/,
);

assert.deepEqual(
  targetsForHost('linux').map((target) => target.pkg_suffix),
  ['linux-x64', 'linux-arm64'],
);
assert.deepEqual(
  targetsForHost('darwin').map((target) => target.pkg_suffix),
  ['darwin-arm64'],
);
assert.throws(() => targetsForHost('win32'), /No npm build targets/);

assert.deepEqual(
  buildStrategyForTarget(
    'linux',
    'x86_64-unknown-linux-gnu',
    linuxX64,
  ),
  { tool: 'cargo', passTarget: false },
);
assert.deepEqual(
  buildStrategyForTarget(
    'linux',
    'x86_64-unknown-linux-gnu',
    linuxArm64,
  ),
  { tool: 'cross', passTarget: true },
);
assert.throws(
  () =>
    buildStrategyForTarget(
      'linux',
      'x86_64-unknown-linux-gnu',
      darwinArm64,
    ),
  /use a darwin runner/,
);
assert.deepEqual(
  buildStrategyForTarget(
    'darwin',
    'x86_64-apple-darwin',
    darwinArm64,
  ),
  { tool: 'cargo', passTarget: true },
);
