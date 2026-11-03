// Round-model regression runner: compiles the extracted ATIF trajectory
// round model (src/utils/roundModel.ts) with the dashboard's tsc so the
// fixtures run against typechecked production code, then runs the behavioral
// suite: direct immutable-input fixtures plus a compiled production viewer
// integration (the real AtifViewerPage transpiled with the dashboard's babel
// toolchain, driven through a minimal hooks driver).
//
// The compile runs with the temp output directory as the working directory:
// TypeScript 5.9+ refuses file arguments while a tsconfig.json sits in the
// working directory (TS5112) and there is no flag both old and new compilers
// accept, so pointing the compiler at a config-free directory (with the
// source passed as an absolute path and --rootDir pinning the output layout)
// keeps the runner working on every compiler version.
const { execFileSync } = require('node:child_process');
const { mkdtempSync, mkdirSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');

const outputDir = mkdtempSync(join(tmpdir(), 'agentsight-round-model-regression-'));

try {
  const typeRoots = join(outputDir, 'types');
  mkdirSync(typeRoots);
  execFileSync(
    'tsc',
    [
      '--typeRoots',
      typeRoots,
      '--outDir',
      outputDir,
      '--rootDir',
      join(process.cwd(), 'src'),
      '--module',
      'commonjs',
      '--target',
      'es2020',
      '--lib',
      'es2020,dom',
      // roundModel.ts type-imports MessageKey from i18n.tsx, so the compiler
      // needs JSX support to resolve it (same as the navigation suite).
      '--jsx',
      'react-jsx',
      '--esModuleInterop',
      '--skipLibCheck',
      join(process.cwd(), 'src', 'utils', 'roundModel.ts'),
    ],
    // A config-free working directory sidesteps TS5112 (see above).
    { cwd: outputDir, stdio: 'inherit' },
  );
  execFileSync('node', ['--test', 'tests/round-model-regression.test.cjs'], {
    env: {
      ...process.env,
      AGENTSIGHT_ROUND_MODEL_BUILD: join(outputDir, 'utils', 'roundModel.js'),
    },
    stdio: 'inherit',
  });
} finally {
  rmSync(outputDir, { force: true, recursive: true });
}
