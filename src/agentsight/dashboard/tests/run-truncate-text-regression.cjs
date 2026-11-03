const { execFileSync } = require('node:child_process');
const { mkdtempSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');

const outputDir = mkdtempSync(join(tmpdir(), 'agentsight-truncate-text-regression-'));

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
      'src/utils/text.ts',
    ],
    { stdio: 'inherit' },
  );
  execFileSync('node', ['--test', 'tests/truncate-text-regression.test.cjs'], {
    env: {
      ...process.env,
      AGENTSIGHT_TRUNCATE_TEXT_BUILD: join(outputDir, 'text.js'),
      NODE_PATH: join(process.cwd(), 'node_modules'),
    },
    stdio: 'inherit',
  });
} finally {
  rmSync(outputDir, { force: true, recursive: true });
}
