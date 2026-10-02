/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import {
  writeFileSync,
  rmSync,
  mkdirSync,
  readdirSync,
  copyFileSync,
  cpSync,
  existsSync,
} from 'node:fs';

let esbuild;
try {
  esbuild = (await import('esbuild')).default;
} catch (_error) {
  console.warn('esbuild not available, skipping bundle step');
  process.exit(0);
}

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const require = createRequire(import.meta.url);
const pkg = require(path.resolve(__dirname, 'package.json'));

// Clean dist directory (cross-platform)
rmSync(path.resolve(__dirname, 'dist'), { recursive: true, force: true });

const external = [
  '@lydell/node-pty',
  'node-pty',
  '@lydell/node-pty-darwin-arm64',
  '@lydell/node-pty-darwin-x64',
  '@lydell/node-pty-linux-arm64',
  '@lydell/node-pty-linux-x64',
  '@lydell/node-pty-win32-arm64',
  '@lydell/node-pty-win32-x64',
  // NOTE: react-devtools-core is intentionally NOT in external.
  // ink v6 has a static ESM import of it for standalone DevTools support
  // (guarded by process.env.DEV === 'true').  Marking it external causes
  // ERR_MODULE_NOT_FOUND at startup in production because the package is not
  // installed alongside the bundle.  We alias it to a no-op shim instead so
  // it gets bundled inline and never needs to be present at runtime.
];

esbuild
  .build({
    entryPoints: ['packages/cli/index.ts'],
    bundle: true,
    outfile: 'dist/cli.js',
    platform: 'node',
    format: 'esm',
    target: 'node20',
    external,
    packages: 'bundle',
    inject: [path.resolve(__dirname, 'scripts/esbuild-shims.js')],
    banner: {
      js: `// Force strict mode and setup for ESM
"use strict";`,
    },
    alias: {
      'is-in-ci': path.resolve(
        __dirname,
        'packages/cli/src/patches/is-in-ci.ts',
      ),
      // Replace react-devtools-core with a no-op shim so the production bundle
      // is self-contained and doesn't need the devtools package at runtime.
      'react-devtools-core': path.resolve(
        __dirname,
        'scripts/shims/react-devtools-core.js',
      ),
    },
    define: {
      'process.env.CLI_VERSION': JSON.stringify(pkg.version),
      // Make global available for compatibility
      global: 'globalThis',
    },
    loader: { '.node': 'file' },
    metafile: true,
    write: true,
    keepNames: true,
  })
  .then(({ metafile }) => {
    if (process.env.DEV === 'true') {
      writeFileSync('./dist/esbuild.json', JSON.stringify(metafile, null, 2));
    }
    // Copy hooks/*.py into dist/hooks/ so installCommand can locate them at runtime
    const hooksSource = path.resolve(__dirname, 'hooks');
    const hooksTarget = path.resolve(__dirname, 'dist', 'hooks');
    if (existsSync(hooksSource)) {
      mkdirSync(hooksTarget, { recursive: true });
      for (const file of readdirSync(hooksSource)) {
        if (file.endsWith('.py')) {
          copyFileSync(
            path.join(hooksSource, file),
            path.join(hooksTarget, file),
          );
        }
      }
    }

    // Copy extension examples into dist/examples/ so extensions new command works at runtime
    const examplesSource = path.resolve(
      __dirname,
      'packages',
      'cli',
      'src',
      'commands',
      'extensions',
      'examples',
    );
    const examplesTarget = path.resolve(__dirname, 'dist', 'examples');
    if (existsSync(examplesSource)) {
      mkdirSync(examplesTarget, { recursive: true });
      for (const entry of readdirSync(examplesSource, {
        withFileTypes: true,
      })) {
        if (entry.isDirectory()) {
          cpSync(
            path.join(examplesSource, entry.name),
            path.join(examplesTarget, entry.name),
            { recursive: true },
          );
        }
      }
    }
  })
  .catch((error) => {
    console.error('esbuild build failed:', error);
    process.exitCode = 1;
  });
