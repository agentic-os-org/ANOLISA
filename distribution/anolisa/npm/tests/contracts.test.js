/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/** Verify the published native payload contains the executable's sibling registry. */
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import {
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir, platform, arch } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { COMPONENT_CONTRACTS, copyIntegrationContracts } from '../scripts/contracts.js';

const npmRoot = fileURLToPath(new URL('..', import.meta.url));
const root = mkdtempSync(join(tmpdir(), 'anolisa-npm-contracts-'));
try {
  const payload = join(root, 'payload');
  mkdirSync(join(payload, 'bin'), { recursive: true });
  writeFileSync(join(payload, 'bin', 'anolisa'), '#!/bin/sh\nexit 0\n');
  copyFileSync(
    join(npmRoot, 'platforms', `${platform()}-${arch()}`, 'package.json'),
    join(payload, 'package.json'),
  );
  copyIntegrationContracts(dirname(npmRoot), payload);
  const packed = spawnSync('npm', ['pack', '--json', '--dry-run', '--ignore-scripts'], {
    cwd: payload,
    encoding: 'utf8',
    timeout: 30000,
    env: {
      ...process.env,
      npm_config_cache: join(root, 'cache'),
      npm_config_offline: 'true',
    },
  });
  assert.ifError(packed.error);
  assert.equal(packed.status, 0, packed.stderr);
  const files = JSON.parse(packed.stdout)[0].files.map((file) => file.path).sort();
  assert.deepEqual(files, [
    'bin/anolisa',
    'package.json',
    ...COMPONENT_CONTRACTS.map(
      (component) => `share/anolisa/components/${component}/component.toml`,
    ),
  ].sort());
  for (const component of COMPONENT_CONTRACTS) {
    assert.equal(
      readFileSync(join(payload, 'share', 'anolisa', 'components', component, 'component.toml'), 'utf8'),
      readFileSync(join(dirname(npmRoot), 'manifests', 'components', component, 'component.toml'), 'utf8'),
    );
  }
  assert.throws(
    () => copyIntegrationContracts(join(root, 'missing-source'), join(root, 'missing-output')),
    /Distribution integration contract missing/,
  );
  console.log('ok - native npm package includes the complete sibling integration registry');
} finally {
  rmSync(root, { recursive: true, force: true });
}
