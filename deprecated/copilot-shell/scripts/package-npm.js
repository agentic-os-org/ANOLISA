/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Automated npm packaging and publishing script for copilot-shell.
 *
 * Usage:
 *   npm run package:npm                      # Package only (creates tarball)
 *   npm run package:npm -- --publish         # Package and publish to registry
 *   npm run package:npm -- --dry-run         # Simulate publish (npm publish --dry-run)
 *   npm run package:npm -- --tag beta        # Publish with a dist-tag
 *
 * Prerequisites:
 *   - Run `npm run bundle` first (or let this script run it)
 *   - npm login completed for @anolisa scope (if publishing)
 *
 * Output:
 *   dist/                                    # Ready-to-publish package directory
 *   *.tgz                                    # npm tarball (from npm pack)
 */

import { execSync } from 'node:child_process';
import { existsSync, readFileSync, rmSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');
const distDir = join(rootDir, 'dist');

// Parse arguments
const args = process.argv.slice(2);
const shouldPublish = args.includes('--publish');
const dryRun = args.includes('--dry-run');
const tagIdx = args.indexOf('--tag');
const distTag = tagIdx !== -1 ? args[tagIdx + 1] : null;

// Read version
const packageJson = JSON.parse(
  readFileSync(join(rootDir, 'package.json'), 'utf-8'),
);
const { name, version } = packageJson;

console.log(`\n📦 npm packaging for ${name}@${version}\n`);

// Step 1: Build if not already built
if (!existsSync(join(distDir, 'cli.js'))) {
  console.log('Step 1/4: Building bundle...');
  execSync('npm run bundle', { stdio: 'inherit', cwd: rootDir });
} else {
  console.log('Step 1/4: Bundle already exists, skipping build.');
}

// Step 2: Prepare package (generates dist/package.json, copies assets)
console.log('\nStep 2/4: Preparing package...');
execSync('npm run prepare:package', { stdio: 'inherit', cwd: rootDir });

// Step 3: Verify package contents
console.log('\nStep 3/4: Verifying package...');
const distPkgPath = join(distDir, 'package.json');
if (!existsSync(distPkgPath)) {
  console.error('Error: dist/package.json not found after prepare:package');
  process.exit(1);
}

const distPkg = JSON.parse(readFileSync(distPkgPath, 'utf-8'));
console.log(`  Name: ${distPkg.name}`);
console.log(`  Version: ${distPkg.version}`);
console.log(`  Bin: ${Object.keys(distPkg.bin || {}).join(', ')}`);
console.log(`  Files: ${(distPkg.files || []).join(', ')}`);

// Verify critical files exist
const criticalFiles = ['cli.js', 'package.json', 'README.md', 'LICENSE'];
for (const file of criticalFiles) {
  if (!existsSync(join(distDir, file))) {
    console.error(`Error: Critical file missing: dist/${file}`);
    process.exit(1);
  }
}
console.log('  ✅ All critical files present');

// Show package size
try {
  const sizeOutput = execSync(`du -sh "${distDir}" | cut -f1`, {
    encoding: 'utf-8',
    cwd: rootDir,
  }).trim();
  console.log(`  Size: ${sizeOutput}`);
} catch {
  // du may not be available on all platforms
}

// Step 4: Pack / Publish
console.log('\nStep 4/4: Creating npm tarball...');

// Remove any existing tarballs
const existingTgz = execSync('ls *.tgz 2>/dev/null || true', {
  encoding: 'utf-8',
  cwd: distDir,
}).trim();
if (existingTgz) {
  for (const tgz of existingTgz.split('\n').filter(Boolean)) {
    rmSync(join(distDir, tgz), { force: true });
  }
}

// Pack
execSync('npm pack', { stdio: 'inherit', cwd: distDir });

if (shouldPublish || dryRun) {
  const publishCmd = ['npm', 'publish'];
  if (dryRun) publishCmd.push('--dry-run');
  if (distTag) publishCmd.push('--tag', distTag);
  publishCmd.push('--access', 'public');

  console.log(
    `\n🚀 ${dryRun ? 'Dry-run' : 'Publishing'}: ${publishCmd.join(' ')}`,
  );
  execSync(publishCmd.join(' '), { stdio: 'inherit', cwd: distDir });

  if (!dryRun) {
    console.log(`\n✅ Published ${name}@${version} to npm`);
  } else {
    console.log(`\n✅ Dry-run complete for ${name}@${version}`);
  }
} else {
  console.log(`\n✅ Package ready at: dist/`);
  console.log('\nTo publish:');
  console.log(`  cd dist && npm publish --access public`);
  if (distTag) {
    console.log(
      `  # or with tag: cd dist && npm publish --access public --tag ${distTag}`,
    );
  }
}

console.log('\nTo install (after publishing):');
console.log(`  npm install -g ${name}`);
console.log(`  # Then run: cosh`);
