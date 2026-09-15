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
  readFileSync,
  writeFileSync,
} from 'node:fs';
import { execSync } from 'node:child_process';
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

// Ownership marker inside the shared adapter directory. Content cannot prove
// ownership — every copy of the same release is byte-identical — so whoever
// places the tree stamps it with who placed it, and a foreign reinstall removes
// the stamp along with the tree it replaces.
const OWNER_MARKER_FILE = '.tokenless-owner';
const NPM_OWNER_PREFIX = 'npm:';
// Written by src/tokenless/scripts/install.sh, which runs this postinstall and
// then claims the tree for its receipt. Same family, so it is replaceable here.
const CURL_OWNER_PREFIX = 'curl-installer:';
// Env escape hatch for a user who really does want this package to take the
// shared directory over.
const FORCE_ADAPTERS_ENV = 'ANOLISA_TOKENLESS_FORCE_ADAPTERS';

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
 * The contract anolisa writes next to the adapter resources when it installs or
 * adopts a component: {datadir}/components/<component>/component.toml, where in
 * user mode {datadir} is the parent of the adapters directory (~/.local/share/
 * anolisa). Its presence means the tree in `dest` belongs to a managed component
 * installation, not to this package.
 */
function anolisaComponentContract(destParent) {
  const dataDir = dirname(destParent);
  const contract = join(dataDir, 'components', 'tokenless', 'component.toml');
  return existsSync(contract) ? contract : null;
}

function readOwnerMarker(dest) {
  try {
    return readFileSync(join(dest, OWNER_MARKER_FILE), 'utf8').split('\n')[0].trim();
  } catch {
    return '';
  }
}

function writeOwnerMarker(dest, pkgVersion) {
  try {
    writeFileSync(join(dest, OWNER_MARKER_FILE), `${NPM_OWNER_PREFIX}anolisa-tokenless@${pkgVersion}\n`);
  } catch {
    // The tree is usable without the marker; the next run then treats it as
    // unowned rather than failing the install over a bookkeeping file.
  }
}

/**
 * Returns a human-readable description of the installation that owns `dest`, or
 * null when this package may replace it. A tree this package or the standalone
 * curl installer placed carries their marker; anything else — an anolisa
 * component install, or a copy somebody made by hand — does not.
 */
function foreignAdapterOwner(dest, destParent) {
  if (!existsSync(dest)) return null;
  const contract = anolisaComponentContract(destParent);
  if (contract) return `an anolisa component installation (${contract})`;
  const marker = readOwnerMarker(dest);
  if (marker && !marker.startsWith(NPM_OWNER_PREFIX) && !marker.startsWith(CURL_OWNER_PREFIX)) {
    return `another installation (owner marker '${marker}')`;
  }
  return null;
}

/**
 * Install the bundled Agent adapters (hook scripts and install helpers —
 * plain bash/python, OS independent) into the user-level data directory that
 * the hook dispatcher (common/hooks/run-hook.sh) already searches:
 *   ~/.local/share/anolisa/adapters/tokenless
 *
 * That directory is shared. Replacing a tree another installation put there
 * would leave its component record and every framework registration (hook
 * entries, plugin directories, symlinks) pointing at resources it no longer has,
 * and nothing about the new copy identifies the owner that was overwritten. So
 * identify the current owner first and preserve a foreign one.
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

  const foreignOwner = foreignAdapterOwner(dest, destParent);
  if (foreignOwner && process.env[FORCE_ADAPTERS_ENV] !== '1') {
    console.warn(`anolisa-tokenless: ${dest} belongs to ${foreignOwner}.`);
    console.warn('anolisa-tokenless: Keeping it unchanged. Replacing it would leave that');
    console.warn("anolisa-tokenless: installation's component record and its framework");
    console.warn('anolisa-tokenless: registrations pointing at resources it no longer has.');
    console.log(`anolisa-tokenless: The adapter resources this package ships are at ${adaptersSrc}`);
    console.log(`anolisa-tokenless: Set ${FORCE_ADAPTERS_ENV}=1 to replace them anyway.`);
    return;
  }
  if (foreignOwner) {
    console.warn(`anolisa-tokenless: ${FORCE_ADAPTERS_ENV}=1 — replacing ${dest},`);
    console.warn(`anolisa-tokenless: which belonged to ${foreignOwner}.`);
  }

  try {
    rmSync(dest, { recursive: true, force: true });
    mkdirSync(destParent, { recursive: true });
    cpSync(adaptersSrc, dest, { recursive: true });
    writeOwnerMarker(dest, readPackageVersion());
    console.log(`anolisa-tokenless: Installed Agent adapters to ${dest}`);
    console.log(
      'anolisa-tokenless: To register an adapter with an Agent product, run its install script, e.g.:',
    );
    console.log(`  bash ${join(dest, 'claude-code', 'scripts', 'install.sh')}`);
  } catch (err) {
    console.warn(`anolisa-tokenless: Could not install adapters to ${dest}: ${err.message}`);
    console.warn(`anolisa-tokenless: Adapter files remain available at ${adaptersSrc}`);
  }
}

function readPackageVersion() {
  try {
    return JSON.parse(readFileSync(join(packageRoot, 'package.json'), 'utf8')).version || 'unknown';
  } catch {
    return 'unknown';
  }
}

main();
