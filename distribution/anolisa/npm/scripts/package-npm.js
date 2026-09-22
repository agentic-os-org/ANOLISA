#!/usr/bin/env node

/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * npm packaging script for @anolisa/cli
 *
 * Builds the Rust binary for the current (or specified) target and packages
 * it into platform-specific npm tarballs ready for `npm publish`.
 *
 * Usage:
 *   node scripts/package-npm.js                     # current platform only
 *   node scripts/package-npm.js --all               # all targets for this OS
 *   node scripts/package-npm.js --target linux-x64  # specific target
 *   node scripts/package-npm.js --validate          # validate package templates
 *
 * Prerequisites:
 *   - Rust toolchain with the target installed (rustup target add ...)
 *   - cargo available on PATH
 *
 * Output:
 *   npm/dist/
 *   ├── anolisa-cli-<version>.tgz                  (root package)
 *   ├── anolisa-cli-linux-x64-<version>.tgz        (platform package)
 *   ├── anolisa-cli-linux-arm64-<version>.tgz      (platform package)
 *   └── anolisa-cli-darwin-arm64-<version>.tgz     (platform package)
 */

import { execSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  copyFileSync,
  rmSync,
} from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { arch, platform } from 'node:os';
import {
  buildStrategyForTarget,
  PLATFORM_MAP,
  TARGETS,
  platformPackageName,
  targetForSelector,
  targetsForHost,
} from './platforms.js';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const npmDir = join(__dirname, '..');
const workspaceRoot = join(npmDir, '..');
const distDir = join(npmDir, 'dist');

// Read version from Cargo.toml
const cargoToml = readFileSync(join(workspaceRoot, 'Cargo.toml'), 'utf-8');
const versionMatch = cargoToml.match(/^version\s*=\s*"([^"]+)"/m);
if (!versionMatch) {
  console.error('Error: Could not parse version from Cargo.toml');
  process.exit(1);
}
const version = versionMatch[1];

function packageTemplate(path, expectedName) {
  if (!existsSync(path)) {
    throw new Error(`npm package template not found: ${path}`);
  }
  const template = JSON.parse(readFileSync(path, 'utf8'));
  if (
    !template ||
    typeof template !== 'object' ||
    template.name !== expectedName
  ) {
    throw new Error(`npm package template has the wrong identity: ${path}`);
  }
  return template;
}

function platformPackageTemplate(path, expectedName) {
  const template = packageTemplate(path, expectedName);
  if (
    template.bin !== undefined ||
    !Array.isArray(template.files) ||
    !template.files.includes('bin/') ||
    template.preferUnplugged !== true
  ) {
    throw new Error(
      `npm platform template must be a command-free native payload: ${path}`,
    );
  }
  return template;
}

function rootPackageTemplate(path) {
  const template = packageTemplate(path, '@anolisa/cli');
  if (
    template.type !== 'module' ||
    template.bin?.anolisa !== 'bin/anolisa' ||
    !Array.isArray(template.files) ||
    !template.files.includes('bin/') ||
    !template.files.includes('scripts/') ||
    template.scripts?.postinstall !== 'node scripts/postinstall.js'
  ) {
    throw new Error(
      `npm root template must package and run the ESM postinstall launcher: ${path}`,
    );
  }
  return template;
}

function validatePackageTemplates() {
  if (Object.keys(PLATFORM_MAP).length !== TARGETS.length) {
    throw new Error('npm target list contains duplicate platform keys');
  }
  for (const target of TARGETS) {
    platformPackageTemplate(
      join(npmDir, 'platforms', target.pkg_suffix, 'package.json'),
      platformPackageName(target.npm_os, target.npm_cpu),
    );
  }
  rootPackageTemplate(join(npmDir, 'package.json'));
}

function parseArgs(hostPlatform, hostArch) {
  const args = process.argv.slice(2);
  if (args.includes('--all')) return targetsForHost(hostPlatform);
  const targetIdx = args.indexOf('--target');
  if (targetIdx !== -1 && args[targetIdx + 1]) {
    try {
      return [targetForSelector(args[targetIdx + 1])];
    } catch (error) {
      console.error(`Error: ${error.message}`);
      process.exit(1);
    }
  }
  const current = TARGETS.find(
    (target) =>
      target.npm_os === hostPlatform && target.npm_cpu === hostArch,
  );
  if (!current) {
    console.error(
      `Error: No target configuration for ${hostPlatform}-${hostArch}`,
    );
    process.exit(1);
  }
  return [current];
}

function buildTarget(target, hostPlatform) {
  console.log(`\n🔨 Building for ${target.rust_target}...`);
  const hostTarget = execSync('rustc -vV', { encoding: 'utf-8' })
    .match(/host: (.+)/)?.[1]
    ?.trim();
  if (!hostTarget) {
    throw new Error('Could not determine the Rust host target');
  }
  const strategy = buildStrategyForTarget(
    hostPlatform,
    hostTarget,
    target,
  );
  const targetArg = strategy.passTarget
    ? ` --target ${target.rust_target}`
    : '';

  const buildCmd = `${strategy.tool} build --release --locked -p anolisa-cli${targetArg}`;

  execSync(buildCmd, { stdio: 'inherit', cwd: workspaceRoot });

  const binaryPath = strategy.passTarget
    ? join(workspaceRoot, 'target', target.rust_target, 'release', 'anolisa')
    : join(workspaceRoot, 'target', 'release', 'anolisa');

  if (!existsSync(binaryPath)) {
    console.error(`Error: Binary not found at ${binaryPath}`);
    process.exit(1);
  }

  return binaryPath;
}

function packagePlatform(target, binaryPath) {
  const pkgName = platformPackageName(target.npm_os, target.npm_cpu);
  const pkgDir = join(distDir, `cli-${target.pkg_suffix}`);

  console.log(`📦 Packaging ${pkgName}@${version}...`);

  // Clean and create package directory
  if (existsSync(pkgDir)) rmSync(pkgDir, { recursive: true });
  mkdirSync(join(pkgDir, 'bin'), { recursive: true });

  // Copy binary
  copyFileSync(binaryPath, join(pkgDir, 'bin', 'anolisa'));
  execSync(`chmod 755 "${join(pkgDir, 'bin', 'anolisa')}"`, { stdio: 'pipe' });

  const template = platformPackageTemplate(
    join(npmDir, 'platforms', target.pkg_suffix, 'package.json'),
    pkgName,
  );
  const pkgJson = {
    ...template,
    version,
    os: [target.npm_os],
    cpu: [target.npm_cpu],
  };
  writeFileSync(join(pkgDir, 'package.json'), JSON.stringify(pkgJson, null, 2) + '\n');

  // Create tarball
  execSync(`npm pack`, { stdio: 'pipe', cwd: pkgDir });
  console.log(`  ✅ ${pkgName}@${version} packaged`);

  return pkgDir;
}

function packageRoot() {
  const rootPkgDir = join(distDir, 'cli');
  console.log(`\n📦 Packaging @anolisa/cli@${version} (root)...`);

  if (existsSync(rootPkgDir)) rmSync(rootPkgDir, { recursive: true });
  mkdirSync(join(rootPkgDir, 'bin'), { recursive: true });
  mkdirSync(join(rootPkgDir, 'scripts'), { recursive: true });

  // Write a stub bin/anolisa that postinstall will replace with a symlink
  const stubScript = `#!/usr/bin/env node
console.error('@anolisa/cli: postinstall has not run yet. Run "npm rebuild @anolisa/cli" to fix.');
process.exit(1);
`;
  writeFileSync(join(rootPkgDir, 'bin', 'anolisa'), stubScript);
  execSync(`chmod 755 "${join(rootPkgDir, 'bin', 'anolisa')}"`, { stdio: 'pipe' });

  for (const script of ['platforms.js', 'postinstall.js']) {
    copyFileSync(
      join(npmDir, 'scripts', script),
      join(rootPkgDir, 'scripts', script),
    );
  }

  // Copy README and LICENSE
  for (const file of ['README.md', 'LICENSE']) {
    const src = join(workspaceRoot, file);
    if (existsSync(src)) copyFileSync(src, join(rootPkgDir, file));
  }

  // The root package is platform-neutral even when this invocation builds a
  // single native package, so its install metadata must cover every target.
  const optionalDeps = {};
  for (const t of TARGETS) {
    optionalDeps[platformPackageName(t.npm_os, t.npm_cpu)] = version;
  }

  const template = rootPackageTemplate(join(npmDir, 'package.json'));
  const rootPkgJson = {
    ...template,
    version,
    os: [...new Set(TARGETS.map((target) => target.npm_os))],
    optionalDependencies: optionalDeps,
  };
  writeFileSync(join(rootPkgDir, 'package.json'), JSON.stringify(rootPkgJson, null, 2) + '\n');

  execSync(`npm pack`, { stdio: 'pipe', cwd: rootPkgDir });
  console.log(`  ✅ @anolisa/cli@${version} packaged`);

  return rootPkgDir;
}

async function main() {
  console.log(`\n🚀 ANOLISA CLI npm packaging (v${version})\n`);

  if (process.argv.includes('--validate')) {
    validatePackageTemplates();
    console.log('✅ npm package templates are valid');
    return;
  }

  // Clean dist
  if (existsSync(distDir)) rmSync(distDir, { recursive: true });
  mkdirSync(distDir, { recursive: true });

  const hostPlatform = platform();
  const targets = parseArgs(hostPlatform, arch());
  console.log(`Targets: ${targets.map((t) => t.pkg_suffix).join(', ')}`);

  // Build and package each platform
  for (const target of targets) {
    const binary = buildTarget(target, hostPlatform);
    packagePlatform(target, binary);
  }

  // Package root
  packageRoot();

  console.log(`\n✅ All packages ready in: ${distDir}/`);
  console.log('\nTo publish:');
  console.log('  cd npm/dist/cli && npm publish --access public');
  for (const t of targets) {
    console.log(`  cd npm/dist/cli-${t.pkg_suffix} && npm publish --access public`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
