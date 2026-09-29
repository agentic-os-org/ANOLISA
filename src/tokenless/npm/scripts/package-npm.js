#!/usr/bin/env node

/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * npm packaging script for anolisa-tokenless
 *
 * Packages prebuilt binaries into platform-specific npm tarballs ready for
 * `npm publish`. This script validates and assembles binaries but never
 * compiles native executables.
 *
 * Usage:
 *   node npm/scripts/package-npm.js                     # current platform only
 *   node npm/scripts/package-npm.js --target linux-x64  # one target
 *   node npm/scripts/package-npm.js --all               # all supported targets
 *
 * Prerequisites:
 *   - Prebuilt tokenless and rtk binaries for each selected target
 *   - npm (the architecture-independent OpenClaw adapter is built here)
 *
 * Output:
 *   npm/dist/
 *   ├── tokenless/anolisa-tokenless-<version>.tgz
 *   ├── tokenless-linux-x64/anolisa-tokenless-linux-x64-<version>.tgz
 *   ├── tokenless-linux-arm64/anolisa-tokenless-linux-arm64-<version>.tgz
 *   ├── tokenless-darwin-x64/anolisa-tokenless-darwin-x64-<version>.tgz
 *   └── tokenless-darwin-arm64/anolisa-tokenless-darwin-arm64-<version>.tgz
 */

import { execFileSync, execSync } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
  copyFileSync,
  rmSync,
  cpSync,
  readdirSync,
  chmodSync,
  closeSync,
  openSync,
  readSync,
  statSync,
} from 'node:fs';
import { join, dirname, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const npmDir = join(__dirname, '..');
const tokenlessRoot = join(npmDir, '..');
const distDir = join(npmDir, 'dist');
const prebuiltRoot = join(tokenlessRoot, 'target', 'npm-prebuilt');

// Read version from workspace Cargo.toml
const cargoToml = readFileSync(join(tokenlessRoot, 'Cargo.toml'), 'utf-8');
const versionMatch = cargoToml.match(/\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"/);
const version = versionMatch ? versionMatch[1] : cargoToml.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!version) {
  console.error('Error: Could not parse version from Cargo.toml');
  process.exit(1);
}

const BINARIES = ['tokenless', 'rtk'];

// npm registry every generated manifest and publish command is pinned to.
// The nested dist/* package roots do NOT inherit npm/.npmrc, so without an
// explicit publishConfig a maintainer's user-level registry would win.
const NPM_REGISTRY = 'https://registry.npmjs.org/';
// Floor advertised in engines.node; keep in sync with npm/package.json.
const NODE_ENGINE_MIN = '16.7.0';

const PUBLISH_CONFIG = { registry: NPM_REGISTRY, access: 'public' };
const GLIBC_MIN = '2.17';

const TARGETS = [
  {
    rust_target: 'x86_64-unknown-linux-gnu',
    npm_os: 'linux',
    npm_cpu: 'x64',
    pkg_suffix: 'linux-x64',
    binary_os: 'linux',
    binary_arch: 'x86_64',
  },
  {
    rust_target: 'aarch64-unknown-linux-gnu',
    npm_os: 'linux',
    npm_cpu: 'arm64',
    pkg_suffix: 'linux-arm64',
    binary_os: 'linux',
    binary_arch: 'aarch64',
  },
  {
    rust_target: 'x86_64-apple-darwin',
    npm_os: 'darwin',
    npm_cpu: 'x64',
    pkg_suffix: 'darwin-x64',
    binary_os: 'macos',
    binary_arch: 'x86_64',
  },
  {
    rust_target: 'aarch64-apple-darwin',
    npm_os: 'darwin',
    npm_cpu: 'arm64',
    pkg_suffix: 'darwin-arm64',
    binary_os: 'macos',
    binary_arch: 'aarch64',
  },
];

function printUsage() {
  console.log(`Usage:
  node npm/scripts/package-npm.js
  node npm/scripts/package-npm.js --target <target>
  node npm/scripts/package-npm.js --all

Targets: ${TARGETS.map((target) => target.pkg_suffix).join(', ')}

Target selectors also accept an OS, npm CPU, architecture, or Rust target
triple and may select more than one matching target.

Prebuilt binaries are read from:
  target/npm-prebuilt/<target>/{tokenless,rtk}`);
}

function optionValue(args, index, option) {
  const value = args[index + 1];
  if (!value || value.startsWith('--')) {
    throw new Error(`${option} requires a value`);
  }
  return value;
}

function parseArgs() {
  const args = process.argv.slice(2);
  let all = false;
  let targetName;

  for (let index = 0; index < args.length; index += 1) {
    const arg = args[index];
    switch (arg) {
      case '--all':
        all = true;
        break;
      case '--target':
        targetName = optionValue(args, index, arg);
        index += 1;
        break;
      case '--help':
      case '-h':
        printUsage();
        process.exit(0);
      default:
        throw new Error(`unknown argument: ${arg}`);
    }
  }

  if (all && targetName) {
    throw new Error('--all and --target cannot be used together');
  }
  if (all) {
    return TARGETS;
  }

  const currentName = `${process.platform}-${process.arch}`;
  const selectedName = targetName || currentName;
  const targets = targetName
    ? TARGETS.filter(
        (candidate) =>
          candidate.rust_target.includes(selectedName) ||
          candidate.npm_cpu === selectedName ||
          candidate.pkg_suffix.includes(selectedName) ||
          candidate.npm_os === selectedName,
      )
    : TARGETS.filter((candidate) => candidate.pkg_suffix === selectedName);
  if (targets.length === 0) {
    throw new Error(
      `unknown target ${selectedName}; available targets: ` +
      TARGETS.map((candidate) => candidate.pkg_suffix).join(', '),
    );
  }
  return targets;
}

function readHeader(path) {
  const descriptor = openSync(path, 'r');
  try {
    const header = Buffer.alloc(64);
    const length = readSync(descriptor, header, 0, header.length, 0);
    return header.subarray(0, length);
  } finally {
    closeSync(descriptor);
  }
}

function verifyBinaryFormat(target, name, path) {
  const header = readHeader(path);
  if (target.binary_os === 'linux') {
    const elfMagic = Buffer.from([0x7f, 0x45, 0x4c, 0x46]);
    if (header.length < 20 || !header.subarray(0, 4).equals(elfMagic)) {
      throw new Error(`${name} is not an ELF binary: ${path}`);
    }
    if (header[4] !== 2 || header[5] !== 1) {
      throw new Error(`${name} is not a 64-bit little-endian ELF binary: ${path}`);
    }
    const expected = target.binary_arch === 'x86_64' ? 62 : 183;
    const actual = header.readUInt16LE(18);
    if (actual !== expected) {
      throw new Error(`${name} ELF machine ${actual} does not match ${target.pkg_suffix}`);
    }
    return;
  }

  if (header.length < 8) throw new Error(`${name} Mach-O header is truncated: ${path}`);
  const littleEndian = header.subarray(0, 4).equals(Buffer.from([0xcf, 0xfa, 0xed, 0xfe]));
  const bigEndian = header.subarray(0, 4).equals(Buffer.from([0xfe, 0xed, 0xfa, 0xcf]));
  if (!littleEndian && !bigEndian) {
    throw new Error(`${name} is not a thin 64-bit Mach-O binary: ${path}`);
  }
  const actual = littleEndian ? header.readUInt32LE(4) : header.readUInt32BE(4);
  const expected = target.binary_arch === 'x86_64' ? 0x01000007 : 0x0100000c;
  if (actual !== expected) {
    throw new Error(
      `${name} Mach-O CPU 0x${actual.toString(16)} does not match ${target.pkg_suffix}`,
    );
  }
}

function compareVersions(left, right) {
  const width = Math.max(left.length, right.length);
  for (let index = 0; index < width; index += 1) {
    const delta = (left[index] || 0) - (right[index] || 0);
    if (delta !== 0) return delta;
  }
  return 0;
}

/** Report when Linux binaries require a newer GLIBC than the package baseline. */
function verifyGlibcBaseline(target, name, path) {
  if (target.binary_os !== 'linux') return;

  let output;
  try {
    output = execFileSync('readelf', ['--version-info', '--wide', path], {
      encoding: 'utf-8',
      maxBuffer: 64 * 1024 * 1024,
    });
  } catch (error) {
    const detail =
      error.code === 'ENOENT' ? 'readelf is not installed' : 'readelf could not inspect it';
    console.warn(
      `  ⚠️  cannot verify the GLIBC ${GLIBC_MIN} baseline for ${name}: ` +
      `${detail}; skipping verification (${path})`,
    );
    return;
  }

  const required = [...output.matchAll(/GLIBC_(\d+)\.(\d+)(?:\.(\d+))?/g)].map((match) =>
    match.slice(1).filter((part) => part !== undefined).map(Number),
  );
  if (required.length === 0) return;

  const maximum = required.reduce((left, right) =>
    compareVersions(left, right) >= 0 ? left : right,
  );
  const baseline = GLIBC_MIN.split('.').map(Number);
  if (compareVersions(maximum, baseline) > 0) {
    console.warn(
      `  ⚠️  ` +
      `${name} requires GLIBC_${maximum.join('.')}, exceeding the supported ` +
      `GLIBC_${GLIBC_MIN} baseline (${path})`,
    );
  }
}

function collectPrebuiltBinaries(target, binDir) {
  const binaryPaths = {};
  for (const binary of BINARIES) {
    const path = join(binDir, binary);
    if (!existsSync(path) || !statSync(path).isFile()) {
      throw new Error(`missing prebuilt ${binary} for ${target.pkg_suffix}: ${path}`);
    }
    verifyBinaryFormat(target, binary, path);
    verifyGlibcBaseline(target, binary, path);
    binaryPaths[binary] = path;
  }
  console.log(`  ✅ Verified prebuilt ${Object.keys(binaryPaths).join(', ')}`);
  return binaryPaths;
}

function packagePlatform(target, binaryPaths) {
  const pkgName = `@anolisa/tokenless-${target.pkg_suffix}`;
  const pkgDir = join(distDir, `tokenless-${target.pkg_suffix}`);

  console.log(`\n📦 Packaging ${pkgName}@${version}...`);

  // Clean and create package directory
  if (existsSync(pkgDir)) rmSync(pkgDir, { recursive: true });
  mkdirSync(join(pkgDir, 'bin'), { recursive: true });

  // Copy binaries
  for (const [bin, binPath] of Object.entries(binaryPaths)) {
    const destination = join(pkgDir, 'bin', bin);
    copyFileSync(binPath, destination);
    chmodSync(destination, 0o755);
  }

  // Write package.json
  //
  // Deliberately declare NO `bin` entries here: the platform packages would
  // otherwise claim the same `tokenless`/`rtk` bin names as the root
  // package. When multiple packages in a tree claim the same bin, npm's
  // reify removes every conflicting `.bin` link instead of picking a
  // winner, leaving installs without a `tokenless` executable. esbuild ships
  // its platform packages the same way (binaries in bin/, no bin field); the
  // root package owns the bin entries and its postinstall links them to
  // these native binaries.
  const archLabel = target.npm_cpu === 'x64' ? 'x86_64' : 'aarch64';
  const pkgJson = {
    name: pkgName,
    version,
    description: `Token-Less native binaries for ${target.npm_os} ${archLabel}`,
    license: 'Apache-2.0',
    repository: {
      type: 'git',
      url: 'git+https://github.com/alibaba/anolisa.git',
      directory: 'src/tokenless',
    },
    os: [target.npm_os],
    cpu: [target.npm_cpu],
    // Binaries target *-unknown-linux-gnu — keep musl (Alpine) installs from
    // matching a package whose ELF they cannot run.
    ...(target.npm_os === 'linux' ? { libc: ['glibc'] } : {}),
    files: ['bin/'],
    preferUnplugged: true,
    publishConfig: PUBLISH_CONFIG,
  };
  writeFileSync(join(pkgDir, 'package.json'), JSON.stringify(pkgJson, null, 2) + '\n');

  // Create tarball
  execSync(`npm pack`, { stdio: 'pipe', cwd: pkgDir });
  console.log(`  ✅ ${pkgName}@${version} packaged`);

  return pkgDir;
}

/** Recursively invoke cb(path) for every regular file under dir. */
function walkFiles(dir, cb) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) walkFiles(p, cb);
    else cb(p);
  }
}

/**
 * Build adapter payloads that are not plain source files. The OpenClaw plugin
 * is TypeScript and must be compiled to dist/index.js before it can be
 * installed by openclaw plugins install; a clean Git checkout only contains
 * index.ts. The dsh entry is plain ESM, but its package manifest is generated
 * from the component version and is validated by the same Makefile seam.
 */
function buildAdapters() {
  const openclawDir = join(tokenlessRoot, 'adapters', 'tokenless', 'openclaw');

  // Always rebuild: the plugin's `npm run build` cleans dist/ first, so a
  // stale or hand-edited dist/index.js left in the (gitignored) work tree can
  // never leak into a published tarball.
  console.log('  Building OpenClaw plugin (TypeScript -> dist/index.js)...');
  try {
    execSync('make build-openclaw-plugin build-dsh-plugin', {
      stdio: 'inherit',
      cwd: tokenlessRoot,
    });
  } catch (err) {
    throw new Error(
      `Adapter bundle preparation failed. Ensure npm and TypeScript are available. ` +
      `Build manually with: make -C src/tokenless build-openclaw-plugin build-dsh-plugin`,
    );
  }

  if (!existsSync(join(openclawDir, 'dist', 'index.js'))) {
    throw new Error(
      `OpenClaw plugin build did not produce adapters/tokenless/openclaw/dist/index.js`,
    );
  }

  const dshDir = join(tokenlessRoot, 'adapters', 'tokenless', 'dsh');
  for (const relative of ['package.json', 'cordis.patch.yml', 'dist/index.js']) {
    if (!existsSync(join(dshDir, relative))) {
      throw new Error(`Native dsh adapter bundle is missing adapters/tokenless/dsh/${relative}`);
    }
  }
}

/**
 * Copy the adapter tree into the root package, mirroring what the Makefile's
 * install-adapter-resources does for FHS installs: strip build-only
 * artifacts, stamp @VERSION@ templates, and mark scripts executable.
 */
function copyAdapters(rootPkgDir) {
  const adaptersSrc = join(tokenlessRoot, 'adapters', 'tokenless');
  const adaptersDest = join(rootPkgDir, 'adapters', 'tokenless');
  const legacyAgentscopeSrc = join(adaptersSrc, 'agentscope');

  console.log('  Bundling Agent adapters...');
  cpSync(adaptersSrc, adaptersDest, {
    recursive: true,
    // npm pack omits symlinks; shared hook dispatchers must ship as regular files.
    dereference: true,
    filter: (src) =>
      src !== legacyAgentscopeSrc &&
      !src.startsWith(`${legacyAgentscopeSrc}${sep}`) &&
      !src.split(sep).includes('node_modules') &&
      !src.endsWith(`${sep}package-lock.json`) &&
      !src.endsWith(`${sep}.gitignore`),
  });

  // Stamp *.in templates with the release version (Makefile does this via
  // stamp-adapter-templates for RPM installs) and drop the raw templates.
  walkFiles(adaptersDest, (p) => {
    if (p.endsWith('.in')) {
      const stamped = readFileSync(p, 'utf-8').replaceAll('@VERSION@', version);
      writeFileSync(p.slice(0, -3), stamped);
      rmSync(p);
    }
  });

  walkFiles(adaptersDest, (p) => {
    if (p.endsWith('.sh') || p.endsWith('.py')) chmodSync(p, 0o755);
  });
}

function packageRoot(targets) {
  const rootPkgDir = join(distDir, 'tokenless');
  console.log(`\n📦 Packaging anolisa-tokenless@${version} (root)...`);

  if (existsSync(rootPkgDir)) rmSync(rootPkgDir, { recursive: true });
  mkdirSync(join(rootPkgDir, 'bin'), { recursive: true });
  mkdirSync(join(rootPkgDir, 'scripts'), { recursive: true });

  // Write stub bin scripts that postinstall will replace with symlinks
  for (const bin of BINARIES) {
    const stubScript = `#!/usr/bin/env node
console.error(
  'anolisa-tokenless: postinstall has not run yet. ' +
  'Run "npm rebuild anolisa-tokenless" to fix.',
);
process.exit(1);
`;
    writeFileSync(join(rootPkgDir, 'bin', bin), stubScript);
    chmodSync(join(rootPkgDir, 'bin', bin), 0o755);
  }

  // Copy postinstall script
  copyFileSync(
    join(npmDir, 'scripts', 'postinstall.js'),
    join(rootPkgDir, 'scripts', 'postinstall.js'),
  );

  // Build and bundle Agent adapters (hook scripts are plain bash/python
  // — OS and architecture independent), so npm installs get adapter
  // integration on macOS and Linux alike. postinstall copies them to the
  // user-level data dir that run-hook.sh already searches
  // (~/.local/share/anolisa/...).
  buildAdapters();
  copyAdapters(rootPkgDir);

  // Copy README and LICENSE
  const readmeSrc = join(tokenlessRoot, 'README.md');
  if (existsSync(readmeSrc)) copyFileSync(readmeSrc, join(rootPkgDir, 'README.md'));

  const licenseSrc = join(tokenlessRoot, 'LICENSE');
  if (existsSync(licenseSrc)) copyFileSync(licenseSrc, join(rootPkgDir, 'LICENSE'));

  // Build optionalDependencies from target list
  const optionalDeps = {};
  for (const t of targets) {
    optionalDeps[`@anolisa/tokenless-${t.pkg_suffix}`] = version;
  }

  // Determine os and cpu arrays from targets
  const osSet = [...new Set(targets.map((t) => t.npm_os))];
  const cpuSet = [...new Set(targets.map((t) => t.npm_cpu))];

  // Build bin map
  const binMap = {};
  for (const bin of BINARIES) {
    binMap[bin] = `bin/${bin}`;
  }

  // Write root package.json
  const rootPkgJson = {
    name: 'anolisa-tokenless',
    type: 'module',
    version,
    description:
      'Token-Less — LLM token optimization toolkit ' +
      '(schema/response compression, command rewriting, tool readiness)',
    license: 'Apache-2.0',
    repository: {
      type: 'git',
      url: 'git+https://github.com/alibaba/anolisa.git',
      directory: 'src/tokenless',
    },
    homepage: 'https://github.com/alibaba/anolisa/tree/main/src/tokenless',
    keywords: ['anolisa', 'tokenless', 'llm', 'token-optimization', 'compression', 'cli'],
    bin: binMap,
    files: ['bin/', 'scripts/', 'adapters/', 'README.md', 'LICENSE'],
    scripts: { postinstall: 'node scripts/postinstall.js' },
    // 16.7.0, not 16.0.0: scripts/postinstall.js imports fs.cpSync, which only
    // exists from 16.7.0. A lower floor lets npm install the package on a node
    // that then fails to load the postinstall script at all, before any of its
    // own platform checks run.
    engines: { node: `>=${NODE_ENGINE_MIN}` },
    os: osSet,
    cpu: cpuSet,
    optionalDependencies: optionalDeps,
    publishConfig: PUBLISH_CONFIG,
  };
  writeFileSync(join(rootPkgDir, 'package.json'), JSON.stringify(rootPkgJson, null, 2) + '\n');

  execSync(`npm pack`, { stdio: 'pipe', cwd: rootPkgDir });
  console.log(`  ✅ anolisa-tokenless@${version} packaged`);

  return rootPkgDir;
}

async function main() {
  console.log(`\n🚀 Token-Less npm packaging (v${version})\n`);

  const targets = parseArgs();
  console.log(`Targets: ${targets.map((target) => target.pkg_suffix).join(', ')}`);

  // Validate the complete input set before replacing an existing dist tree.
  const verified = targets.map((target) => {
    const binDir = join(prebuiltRoot, target.pkg_suffix);
    console.log(`\n🔎 Verifying ${target.pkg_suffix} binaries from ${binDir}...`);
    return [target, collectPrebuiltBinaries(target, binDir)];
  });

  if (existsSync(distDir)) rmSync(distDir, { recursive: true });
  mkdirSync(distDir, { recursive: true });

  for (const [target, binaryPaths] of verified) {
    packagePlatform(target, binaryPaths);
  }

  // The root package always advertises every published platform package, even
  // when only one selected platform package is assembled locally.
  packageRoot(TARGETS);

  console.log(`\n✅ Selected packages ready in: ${distDir}/`);
  console.log('\nTo publish (platform packages first, root last; registry pinned to npmjs):');
  console.log('  make npm-publish');
  console.log('or manually:');
  for (const t of targets) {
    console.log(
      `  cd npm/dist/tokenless-${t.pkg_suffix} && ` +
      `npm publish --access public --registry=${NPM_REGISTRY}`,
    );
  }
  console.log(`  cd npm/dist/tokenless && npm publish --access public --registry=${NPM_REGISTRY}`);
}

main().catch((err) => {
  console.error(`Error: ${err.message}`);
  process.exit(1);
});
