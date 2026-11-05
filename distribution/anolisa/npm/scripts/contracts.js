/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/** Distribution-owned integration metadata shipped beside each native binary. */
import { chmodSync, copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

export const COMPONENT_CONTRACTS = [
  'cosh-ng',
  'sec-core',
  'agentsight',
  'tokenless',
  'skillfs',
  'ws-ckpt',
  'agent-memory',
];

export function copyIntegrationContracts(sourceRoot, packageRoot) {
  for (const component of COMPONENT_CONTRACTS) {
    const source = join(
      sourceRoot, 'manifests', 'components', component, 'component.toml',
    );
    if (!existsSync(source)) {
      throw new Error(`Distribution integration contract missing: ${source}`);
    }
  }
  for (const component of COMPONENT_CONTRACTS) {
    const directory = join(packageRoot, 'share', 'anolisa', 'components', component);
    mkdirSync(directory, { recursive: true });
    const destination = join(directory, 'component.toml');
    copyFileSync(
      join(sourceRoot, 'manifests', 'components', component, 'component.toml'),
      destination,
    );
    chmodSync(destination, 0o644);
  }
}
