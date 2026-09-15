#!/usr/bin/env node

/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * postinstall script for anolisa-tokenless
 *
 * Resolves the platform-specific binary package and creates launcher
 * symlinks at bin/ that delegate to the native binaries, then installs the
 * bundled Agent adapters into the user-level data directory searched by
 * the adapter hook dispatcher (run-hook.sh).
 *
 * Platform packages follow the naming convention:
 *   @anolisa/tokenless-{os}-{arch}
 *
 * Each platform package ships two native binaries:
 *   bin/tokenless, bin/rtk
 *
 * Exit codes: on a supported platform, a missing platform package or a
 * missing binary is a hard failure (non-zero exit) so `npm install` fails
 * loudly instead of leaving broken bin stubs behind. Unsupported platforms
 * are already rejected by the root package's os/cpu constraints.
 */

import {
  existsSync,
  mkdirSync,
  symlinkSync,
  unlinkSync,
  chmodSync,
  cpSync,
  rmSync,
} from 'node:fs';
import { execSync, execFileSync } from 'node:child_process';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { platform, arch, homedir } from 'node:os';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const require = createRequire(import.meta.url);
const packageRoot = join(__dirname, '..');
const binDir = join(packageRoot, 'bin');

const BINARIES = ['tokenless', 'rtk'];

// Map Node.js platform/arch to package names
const PLATFORM_MAP = {
  'linux-x64': '@anolisa/tokenless-linux-x64',
  'linux-arm64': '@anolisa/tokenless-linux-arm64',
  'darwin-x64': '@anolisa/tokenless-darwin-x64',
  'darwin-arm64': '@anolisa/tokenless-darwin-arm64',
};

function resolvePackageDir(pkgName) {
  // Resolve platform package using createRequire (compatible with Node 16+)
  try {
    const resolved = require.resolve(`${pkgName}/package.json`);
    return dirname(resolved);
  } catch {
    // Fallback: walk up to find node_modules
    let current = packageRoot;
    while (current !== dirname(current)) {
      const candidate = join(current, 'node_modules', ...pkgName.split('/'));
      if (existsSync(candidate)) {
        return candidate;
      }
      current = dirname(current);
    }
  }
  return null;
}

function isMusl() {
  // The platform packages declare libc=glibc, but npm CLI versions prior to
  // ~8.3 (e.g. 8.19.4) do not check the libc field in checkPlatform. On a
  // musl-based distribution such as Alpine, the linux-x64 package will still
  // be installed and its glibc-linked ELF will not run. Detect this at
  // postinstall time and fail with a clear message instead of leaving broken
  // bin stubs.
  if (platform() !== 'linux') return false;
  try {
    const out = execSync('ldd --version 2>&1 || true', {
      encoding: 'utf-8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return /\bmusl\b/i.test(out);
  } catch {
    return false;
  }
}

function main() {
  const key = `${platform()}-${arch()}`;
  const pkgName = PLATFORM_MAP[key];

  if (!pkgName) {
    console.warn(
      `anolisa-tokenless: No prebuilt binary available for ${platform()}-${arch()}.`,
    );
    console.warn('You can build from source: https://github.com/alibaba/anolisa/tree/main/src/tokenless');
    process.exit(0);
  }

  if (isMusl()) {
    console.error(
      'anolisa-tokenless: musl-based Linux distributions (e.g. Alpine) are not supported by the prebuilt binaries.',
    );
    console.error(
      'Build from source for your libc: https://github.com/alibaba/anolisa/tree/main/src/tokenless',
    );
    process.exit(1);
  }

  const pkgDir = resolvePackageDir(pkgName);

  if (!pkgDir || !existsSync(pkgDir)) {
    console.error(
      `anolisa-tokenless: Platform package ${pkgName} not found.`,
    );
    console.error(
      'This usually means optionalDependencies were skipped during install.',
    );
    console.error(
      'Try: npm install --include=optional  or  check npm install logs for details.',
    );
    console.error(
      'You can also build from source: https://github.com/alibaba/anolisa/tree/main/src/tokenless',
    );
    // The platform is supported, so the platform package is effectively a
    // required dependency — fail the install instead of leaving bin stubs.
    process.exit(1);
  }

  // Ensure bin/ directory exists
  if (!existsSync(binDir)) {
    mkdirSync(binDir, { recursive: true });
  }

  const missing = [];
  for (const binary of BINARIES) {
    const nativeBinary = join(pkgDir, 'bin', binary);
    if (!existsSync(nativeBinary)) {
      console.error(`anolisa-tokenless: Binary ${binary} not found in ${pkgName}`);
      missing.push(binary);
      continue;
    }

    const linkPath = join(binDir, binary);

    // Remove existing symlink or file
    if (existsSync(linkPath)) {
      unlinkSync(linkPath);
    }

    symlinkSync(nativeBinary, linkPath);
    chmodSync(linkPath, 0o755);
  }

  if (missing.length > 0) {
    console.error(
      `anolisa-tokenless: Incomplete platform package ${pkgName} — missing: ${missing.join(', ')}. Failing install.`,
    );
    process.exit(1);
  }

  console.log(`anolisa-tokenless: Linked ${BINARIES.length} binaries for ${platform()}-${arch()}`);

  installAdapters();
}

/**
 * Enable the Claude Code adapter by running its install helper against the
 * user-level adapter tree. The helper is idempotent and exits cleanly when
 * the `claude` CLI is not present, so this is safe to run on every install.
 *
 * Fail-open: if enabling fails (e.g. the claude CLI is present but plugin
 * validation fails), warn the user and leave the adapter files in place so
 * they can retry manually.
 */
function enableClaudeAdapter(adapterDir) {
  const installScript = join(adapterDir, 'claude-code', 'scripts', 'install.sh');
  if (!existsSync(installScript)) {
    return;
  }

  const claudeBin = process.env.CLAUDE_BIN || 'claude';
  try {
    const out = execFileSync('bash', [installScript], {
      env: { ...process.env, CLAUDE_BIN: claudeBin },
      stdio: 'pipe',
    }).toString();
    // install.sh exits 0 but prints a "skipping" line when claude CLI is absent.
    if (out.includes('skipping plugin installation')) {
      console.log(
        `anolisa-tokenless: claude-code adapter skipped (${claudeBin} not found)`,
      );
      console.log(
        `anolisa-tokenless: Install Claude Code, then run: bash ${installScript}`,
      );
    } else {
      console.log(`anolisa-tokenless: Enabled claude-code adapter (CLAUDE_BIN=${claudeBin})`);
    }
  } catch (err) {
    console.warn(
      `anolisa-tokenless: Could not enable claude-code adapter: ${err.message}`,
    );
    console.warn(
      `anolisa-tokenless: Prerequisite: the claude CLI must be installed and reachable` +
      ` (CLAUDE_BIN=${claudeBin}).`,
    );
    console.warn('anolisa-tokenless: Once the prerequisite is met, re-enable with:');
    console.warn(`  bash ${installScript}`);
  }
}

/**
 * Install the bundled Agent adapters (hook scripts and install helpers —
 * plain bash/python, OS independent) into the user-level data directory that
 * the hook dispatcher (common/hooks/run-hook.sh) already searches:
 *   ~/.local/share/anolisa/adapters/tokenless
 *
 * Fail-open: adapter installation is supplementary — a failure here warns
 * but never fails the npm install, and the files remain available inside
 * the package under adapters/.
 */
function installAdapters() {
  const adaptersSrc = join(packageRoot, 'adapters', 'tokenless');
  if (!existsSync(adaptersSrc)) return;

  const destParent = join(homedir(), '.local', 'share', 'anolisa', 'adapters');
  const dest = join(destParent, 'tokenless');
  try {
    rmSync(dest, { recursive: true, force: true });
    mkdirSync(destParent, { recursive: true });
    cpSync(adaptersSrc, dest, { recursive: true });
    console.log(`anolisa-tokenless: Installed Agent adapters to ${dest}`);
    enableClaudeAdapter(dest);
  } catch (err) {
    console.warn(`anolisa-tokenless: Could not install adapters to ${dest}: ${err.message}`);
    console.warn(`anolisa-tokenless: Adapter files remain available at ${adaptersSrc}`);
  }
}

main();
