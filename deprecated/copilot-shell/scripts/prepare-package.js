/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { execSync } from 'node:child_process';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');

const distDir = path.join(rootDir, 'dist');
const cliBundlePath = path.join(distDir, 'cli.js');
const vendorDir = path.join(distDir, 'vendor');

if (!fs.existsSync(distDir)) {
  console.error('Error: dist/ directory not found');
  console.error('Please run "npm run bundle" first');
  process.exit(1);
}

if (!fs.existsSync(cliBundlePath)) {
  console.error(`Error: Bundle not found at ${cliBundlePath}`);
  console.error('Please run "npm run bundle" first');
  process.exit(1);
}

if (!fs.existsSync(vendorDir)) {
  console.error(`Error: Vendor directory not found at ${vendorDir}`);
  console.error('Please run "npm run bundle" first');
  process.exit(1);
}

console.log('Copying documentation files...');
const filesToCopy = ['README.md', 'LICENSE'];
for (const file of filesToCopy) {
  const sourcePath = path.join(rootDir, file);
  const destPath = path.join(distDir, file);
  if (fs.existsSync(sourcePath)) {
    fs.copyFileSync(sourcePath, destPath);
    console.log(`Copied ${file}`);
  } else {
    console.warn(`Warning: ${file} not found at ${sourcePath}`);
  }
}

console.log('Copying locales folder...');
const localesSourceDir = path.join(
  rootDir,
  'packages',
  'cli',
  'src',
  'i18n',
  'locales',
);
const localesDestDir = path.join(distDir, 'locales');

if (fs.existsSync(localesSourceDir)) {
  function copyRecursiveSync(src, dest) {
    const stats = fs.statSync(src);
    if (stats.isDirectory()) {
      if (!fs.existsSync(dest)) {
        fs.mkdirSync(dest, { recursive: true });
      }
      const entries = fs.readdirSync(src);
      for (const entry of entries) {
        const srcPath = path.join(src, entry);
        const destPath = path.join(dest, entry);
        copyRecursiveSync(srcPath, destPath);
      }
    } else {
      fs.copyFileSync(src, dest);
    }
  }

  copyRecursiveSync(localesSourceDir, localesDestDir);
  console.log('Copied locales folder');
} else {
  console.warn(`Warning: locales folder not found at ${localesSourceDir}`);
}

console.log('Copying @lydell/node-pty native modules...');
const distNodeModulesDir = path.join(distDir, 'node_modules');
const rootNodeModulesDir = path.join(rootDir, 'node_modules');

const ptyPackages = [
  '@lydell/node-pty',
  '@lydell/node-pty-darwin-arm64',
  '@lydell/node-pty-darwin-x64',
  '@lydell/node-pty-linux-arm64',
  '@lydell/node-pty-linux-x64',
  '@lydell/node-pty-win32-arm64',
  '@lydell/node-pty-win32-x64',
  'node-pty',
];

function copyRecursiveSyncForPty(src, dest) {
  const stats = fs.statSync(src);
  if (stats.isDirectory()) {
    if (!fs.existsSync(dest)) {
      fs.mkdirSync(dest, { recursive: true });
    }
    for (const entry of fs.readdirSync(src)) {
      copyRecursiveSyncForPty(path.join(src, entry), path.join(dest, entry));
    }
  } else {
    fs.copyFileSync(src, dest);
    // Preserve executable bit for native binaries
    if (stats.mode & 0o111) {
      fs.chmodSync(dest, stats.mode);
    }
  }
}

function fetchPackageViaNpm(pkgName, version, dest) {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'pty-pack-'));
  try {
    const pkgSpec = `${pkgName}@${version}`;
    console.log(`  Fetching ${pkgSpec} via npm pack...`);
    execSync(`npm pack ${pkgSpec} --pack-destination "${tmpDir}"`, {
      stdio: 'pipe',
      cwd: tmpDir,
    });
    const tarballs = fs.readdirSync(tmpDir).filter((f) => f.endsWith('.tgz'));
    if (tarballs.length === 0) {
      console.warn(`  Warning: npm pack produced no tarball for ${pkgSpec}`);
      return false;
    }
    const tarball = path.join(tmpDir, tarballs[0]);
    fs.mkdirSync(dest, { recursive: true });
    execSync(`tar -xzf "${tarball}" --strip-components=1 -C "${dest}"`, {
      stdio: 'pipe',
    });
    console.log(`  Fetched and extracted ${pkgSpec}`);
    return true;
  } catch (err) {
    console.warn(`  Warning: Failed to fetch ${pkgName}: ${err.message}`);
    return false;
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

const rootPackageJson = JSON.parse(
  fs.readFileSync(path.join(rootDir, 'package.json'), 'utf-8'),
);
const optionalVersions = rootPackageJson.optionalDependencies ?? {};

for (const pkg of ptyPackages) {
  const srcPkg = path.join(rootNodeModulesDir, pkg);
  const destPkg = path.join(distNodeModulesDir, pkg);
  if (fs.existsSync(srcPkg)) {
    copyRecursiveSyncForPty(srcPkg, destPkg);
    console.log(`Copied ${pkg}`);
  } else {
    const version = optionalVersions[pkg];
    if (version) {
      fetchPackageViaNpm(pkg, version, destPkg);
    } else {
      console.warn(
        `Warning: ${pkg} not found locally and no version info, skipping.`,
      );
    }
  }
}
console.log('Done copying node-pty modules.');

console.log('Creating package.json for distribution...');

const distBin = Object.fromEntries(
  Object.entries(rootPackageJson.bin ?? {}).map(([cmd, p]) => [
    cmd,
    path.basename(p),
  ]),
);

const distPackageJson = {
  name: rootPackageJson.name,
  version: rootPackageJson.version,
  description:
    rootPackageJson.description || 'Copilot Shell - Entrance to AI-native OS',
  repository: rootPackageJson.repository,
  type: 'module',
  main: 'cli.js',
  bin: distBin,
  files: [
    'cli.js',
    'vendor',
    'node_modules',
    '*.sb',
    'README.md',
    'LICENSE',
    'locales',
    'hooks',
    'examples',
  ],
  config: rootPackageJson.config,
  dependencies: {},
  optionalDependencies: rootPackageJson.optionalDependencies ?? {},
  engines: rootPackageJson.engines,
};

fs.writeFileSync(
  path.join(distDir, 'package.json'),
  JSON.stringify(distPackageJson, null, 2) + '\n',
);

console.log('\n✅ Package prepared for publishing at dist/');
console.log('\nPackage structure:');
execSync('ls -lh dist/', { stdio: 'inherit', cwd: rootDir });
