const { execFileSync } = require('node:child_process');
const { mkdtempSync, mkdirSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join } = require('node:path');

const outputDir = mkdtempSync(join(tmpdir(), 'agentsight-api-client-regression-'));

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
      // containmentLifecycle.ts type-imports MessageKey from i18n.tsx, so the
      // compiler needs JSX support to resolve that module.
      '--jsx',
      'react-jsx',
      '--esModuleInterop',
      'src/utils/apiClient.ts',
      'src/utils/containmentLifecycle.ts',
      'src/utils/datetime.ts',
      'src/utils/accuracyAttribution.ts',
      'src/utils/semanticSearchFilter.ts',
      'src/pages/security/utils.ts',
      'tests/apiClient-globals.d.ts',
    ],
    { stdio: 'inherit' },
  );
  execFileSync('node', ['--test', 'tests/apiClient-regression.test.cjs'], {
    env: {
      ...process.env,
      AGENTSIGHT_API_CLIENT_BUILD: join(outputDir, 'utils', 'apiClient.js'),
      AGENTSIGHT_CONTAINMENT_LIFECYCLE_BUILD: join(outputDir, 'utils', 'containmentLifecycle.js'),
      AGENTSIGHT_DATETIME_BUILD: join(outputDir, 'utils', 'datetime.js'),
      AGENTSIGHT_ACCURACY_ATTRIBUTION_BUILD: join(outputDir, 'utils', 'accuracyAttribution.js'),
      AGENTSIGHT_SEMANTIC_FILTER_BUILD: join(outputDir, 'utils', 'semanticSearchFilter.js'),
      AGENTSIGHT_SECURITY_UTILS_BUILD: join(outputDir, 'pages', 'security', 'utils.js'),
    },
    stdio: 'inherit',
  });
} finally {
  rmSync(outputDir, { force: true, recursive: true });
}
