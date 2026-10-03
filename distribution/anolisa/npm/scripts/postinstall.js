#!/usr/bin/env node

/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * postinstall script for @anolisa/cli
 *
 * Resolves the platform-specific binary package and creates a launcher
 * script at bin/anolisa that delegates to the native binary.
 *
 * Platform packages follow the naming convention:
 *   @anolisa/cli-{os}-{arch}
 *
 * Each platform package ships a single native binary at:
 *   bin/anolisa
 */

import { existsSync, mkdirSync, symlinkSync, unlinkSync, chmodSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { platform, arch } from 'node:os';
import { platformPackageName } from './platforms.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const require = createRequire(import.meta.url);
const packageRoot = join(__dirname, '..');
const binDir = join(packageRoot, 'bin');

function resolvePackageBinary() {
  const pkgName = platformPackageName(platform(), arch());

  // Resolve platform package using createRequire (compatible with Node 16+)
  let pkgDir;
  try {
    const resolved = require.resolve(`${pkgName}/package.json`);
    pkgDir = dirname(resolved);
  } catch {
    // Fallback: walk up to find node_modules
    let current = packageRoot;
    while (current !== dirname(current)) {
      const candidate = join(current, 'node_modules', ...pkgName.split('/'));
      if (existsSync(candidate)) {
        pkgDir = candidate;
        break;
      }
      current = dirname(current);
    }
  }

  if (!pkgDir || !existsSync(pkgDir)) {
    throw new Error(
      `Platform package ${pkgName} not found; optional dependencies may have been skipped`,
    );
  }

  const nativeBinary = join(pkgDir, 'bin', 'anolisa');
  if (!existsSync(nativeBinary)) {
    throw new Error(
      `Binary not found in ${pkgName} at ${nativeBinary}`,
    );
  }

  return nativeBinary;
}

function main() {
  const nativeBinary = resolvePackageBinary();

  // Ensure bin/ directory exists
  if (!existsSync(binDir)) {
    mkdirSync(binDir, { recursive: true });
  }

  const linkPath = join(binDir, 'anolisa');

  // Remove existing symlink or file
  if (existsSync(linkPath)) {
    unlinkSync(linkPath);
  }

  // Create symlink to the platform-specific binary
  symlinkSync(nativeBinary, linkPath);
  chmodSync(linkPath, 0o755);

  console.log(`@anolisa/cli: Linked native binary for ${platform()}-${arch()}`);
}

try {
  main();
} catch (error) {
  console.error(`@anolisa/cli: ${error.message}`);
  process.exit(1);
}
