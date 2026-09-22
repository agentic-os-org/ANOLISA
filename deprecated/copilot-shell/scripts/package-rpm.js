/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Automated RPM packaging script for copilot-shell
 * Usage: npm run package:rpm
 */

import { execSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  copyFileSync,
  readFileSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';

const __dirname = dirname(fileURLToPath(import.meta.url));
const rootDir = join(__dirname, '..');

// Read package.json to get version
const packageJson = JSON.parse(
  readFileSync(join(rootDir, 'package.json'), 'utf-8'),
);
const version = packageJson.version;
const name = 'copilot-shell';

// Read RPM version from spec file (may differ from package.json, e.g. nightly builds with ~)
// If spec file does not exist, generate it from the .spec.in template.
const specFile = join(rootDir, 'copilot-shell.spec');
const specInFile = join(rootDir, 'copilot-shell.spec.in');
let rpmVersion = version;

if (!existsSync(specFile)) {
  if (!existsSync(specInFile)) {
    console.error(
      'Error: Neither copilot-shell.spec nor copilot-shell.spec.in found',
    );
    process.exit(1);
  }
  console.log(
    `Generating copilot-shell.spec from template (version=${version})...`,
  );
  const specContent = readFileSync(specInFile, 'utf-8').replace(
    /@VERSION@/g,
    version,
  );
  writeFileSync(specFile, specContent, 'utf-8');
}

const specContent = readFileSync(specFile, 'utf-8');
const versionMatch = specContent.match(/^Version:\s*(.+)$/m);
if (versionMatch) {
  rpmVersion = versionMatch[1].trim();
} else {
  console.warn(
    'Warning: Could not parse Version field from copilot-shell.spec, falling back to package.json version',
  );
}

console.log(
  `\n📦 Building RPM package for ${name}-${rpmVersion} (package.json: ${version})\n`,
);

// Step 0: Install dependencies if node_modules is missing or incomplete
console.log('Step 0/6: Installing dependencies...');
execSync('npm ci --ignore-scripts', { stdio: 'inherit', cwd: rootDir });

// Step 1: Bundle the project
console.log('\nStep 1/6: Bundling project...');
execSync('npm run bundle', { stdio: 'inherit', cwd: rootDir });

// Step 2: Prepare package
console.log('\nStep 2/6: Preparing package...');
execSync('npm run prepare:package', { stdio: 'inherit', cwd: rootDir });

// Step 3: Setup rpmbuild directories
console.log('\nStep 3/6: Setting up rpmbuild directories...');
const rpmbuildDir = join(homedir(), 'rpmbuild');
const sourcesDir = join(rpmbuildDir, 'SOURCES');
const specsDir = join(rpmbuildDir, 'SPECS');
const rpmsDir = join(rpmbuildDir, 'RPMS');

for (const dir of [sourcesDir, specsDir, rpmsDir]) {
  if (!existsSync(dir)) {
    mkdirSync(dir, { recursive: true });
  }
}

// Step 4/6: Create source tarball
console.log('\nStep 4/6: Creating source tarball...');
const tarballName = `${name}-${rpmVersion}.tar.gz`;
const tarballPath = join(sourcesDir, tarballName);

// Create tarball with proper directory structure
const excludes = [
  '--exclude=.git',
  '--exclude=.aoneci',
  '--exclude=.copilot-shell',
  '--exclude=.husky',
  '--exclude=.qoder',
  '--exclude=.vscode',
  '--exclude=*/node_modules',
  '--exclude=*/dist',
  '--exclude=*/coverage',
  '--exclude=docs*',
  '--exclude=integration-tests',
  '--exclude=*/junit.xml',
  '--exclude=*/*.test.ts',
  '--exclude=*.tar.gz',
  '--exclude=.DS_Store',
].join(' ');

execSync(
  `tar -czvf "${tarballPath}" ${excludes} --transform='s,^\\.,${name}-${rpmVersion},' .`,
  { stdio: 'inherit', cwd: rootDir },
);

console.log(`Created: ${tarballPath}`);

// Step 5: Copy spec file
console.log('\nStep 5/6: Copying spec file...');
copyFileSync(specFile, join(specsDir, 'copilot-shell.spec'));
console.log(`Copied spec file to: ${specsDir}`);

// Step 6: Build RPM
console.log('\nStep 6/6: Building RPM package...');
try {
  execSync(`rpmbuild -ba ${join(specsDir, 'copilot-shell.spec')}`, {
    stdio: 'inherit',
    cwd: rootDir,
  });
} catch (_error) {
  console.error('\n❌ RPM build failed');
  console.error('Make sure rpmbuild is installed: sudo yum install rpm-build');
  process.exit(1);
}

// Find and display the built RPM
console.log('\n✅ RPM package built successfully!');
console.log('\nOutput location:');
execSync(`find ${rpmsDir} -name "*.rpm" -type f`, { stdio: 'inherit' });
