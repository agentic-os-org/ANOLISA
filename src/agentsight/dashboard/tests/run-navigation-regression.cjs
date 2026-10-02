const { execFileSync } = require('node:child_process');
const { mkdtempSync, mkdirSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');

const outputDir = mkdtempSync(join(tmpdir(), 'agentsight-navigation-regression-'));

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
      '--module',
      'commonjs',
      '--target',
      'es2020',
      '--lib',
      'es2020,dom',
      // navigation.ts type-imports MessageKey from i18n.tsx and AppCapability
      // from apiClient.ts, so the compiler needs JSX support to resolve them,
      // and apiClient.ts reads process.env, which the globals stub declares.
      '--jsx',
      'react-jsx',
      '--esModuleInterop',
      '--skipLibCheck',
      'src/utils/navigation.ts',
      'tests/apiClient-globals.d.ts',
    ],
    { stdio: 'inherit' },
  );
  execFileSync('node', ['--test', 'tests/navigation-regression.test.cjs'], {
    env: {
      ...process.env,
      AGENTSIGHT_NAVIGATION_BUILD: join(outputDir, 'utils', 'navigation.js'),
    },
    stdio: 'inherit',
  });
} finally {
  rmSync(outputDir, { force: true, recursive: true });
}
