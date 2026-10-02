const { execFileSync } = require('node:child_process');
const { mkdtempSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');

const outputDir = mkdtempSync(join(tmpdir(), 'agentsight-i18n-regression-'));

try {
  execFileSync(
    'tsc',
    [
      '--outDir',
      outputDir,
      '--module',
      'commonjs',
      '--target',
      'es2020',
      '--lib',
      'es2020,dom',
      '--jsx',
      'react-jsx',
      '--esModuleInterop',
      '--skipLibCheck',
      'src/i18n.tsx',
    ],
    { stdio: 'inherit' },
  );
  execFileSync('node', ['--test', 'tests/i18n-regression.test.cjs'], {
    env: {
      ...process.env,
      AGENTSIGHT_I18N_BUILD: join(outputDir, 'i18n.js'),
      NODE_PATH: join(process.cwd(), 'node_modules'),
    },
    stdio: 'inherit',
  });
} finally {
  rmSync(outputDir, { force: true, recursive: true });
}
