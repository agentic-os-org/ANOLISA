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
      'src/utils/clipboard.ts',
      'src/utils/containmentLifecycle.ts',
      'src/utils/datetime.ts',
      'src/utils/accuracyAttribution.ts',
      'src/utils/semanticSearchFilter.ts',
      'src/utils/timeseriesBuckets.ts',
      'src/utils/formatDuration.ts',
      'src/utils/richText.tsx',
      'src/utils/savingsCsv.ts',
      'src/utils/sessionModel.ts',
      'src/pages/AgentSessionsPage.tsx',
      'src/utils/setSelection.ts',
      'src/pages/security/utils.ts',
      'src/pages/LoginPage.tsx',
      'src/components/OptimizationSettings.tsx',
      'src/pages/AtifViewerPage.tsx',
      'src/utils/trajectoryTree.ts',
      'src/utils/trajectoryTextFilter.ts',
      'src/utils/roundModel.ts',
      'tests/apiClient-globals.d.ts',
    ],
    { stdio: 'inherit' },
  );
  execFileSync('node', ['--test', 'tests/apiClient-regression.test.cjs', 'tests/login-regression.test.cjs', 'tests/savings-csv-regression.test.cjs', 'tests/session-model-regression.test.cjs', 'tests/llm-config-regression.test.cjs', 'tests/trajectory-filter-regression.test.cjs', 'tests/atif-shape-regression.test.cjs', 'tests/clipboard-regression.test.cjs'], {
    env: {
      ...process.env,
      AGENTSIGHT_API_CLIENT_BUILD: join(outputDir, 'utils', 'apiClient.js'),
      AGENTSIGHT_CLIPBOARD_BUILD: join(outputDir, 'utils', 'clipboard.js'),
      AGENTSIGHT_CONTAINMENT_LIFECYCLE_BUILD: join(outputDir, 'utils', 'containmentLifecycle.js'),
      AGENTSIGHT_DATETIME_BUILD: join(outputDir, 'utils', 'datetime.js'),
      AGENTSIGHT_ACCURACY_ATTRIBUTION_BUILD: join(outputDir, 'utils', 'accuracyAttribution.js'),
      AGENTSIGHT_SEMANTIC_FILTER_BUILD: join(outputDir, 'utils', 'semanticSearchFilter.js'),
      AGENTSIGHT_TIMESERIES_BUCKETS_BUILD: join(outputDir, 'utils', 'timeseriesBuckets.js'),
      AGENTSIGHT_FORMAT_DURATION_BUILD: join(outputDir, 'utils', 'formatDuration.js'),
      AGENTSIGHT_SESSION_MODEL_BUILD: join(outputDir, 'utils', 'sessionModel.js'),
      AGENTSIGHT_SESSION_PAGE_BUILD: join(outputDir, 'pages', 'AgentSessionsPage.js'),
      AGENTSIGHT_RICH_TEXT_BUILD: join(outputDir, 'utils', 'richText.js'),
      AGENTSIGHT_SAVINGS_CSV_BUILD: join(outputDir, 'utils', 'savingsCsv.js'),
      AGENTSIGHT_SET_SELECTION_BUILD: join(outputDir, 'utils', 'setSelection.js'),
      AGENTSIGHT_SECURITY_UTILS_BUILD: join(outputDir, 'pages', 'security', 'utils.js'),
      AGENTSIGHT_LOGIN_PAGE_BUILD: join(outputDir, 'pages', 'LoginPage.js'),
      AGENTSIGHT_LLM_CONFIG_BUILD: join(outputDir, 'components', 'OptimizationSettings.js'),
      AGENTSIGHT_ATIF_PAGE_BUILD: join(outputDir, 'pages', 'AtifViewerPage.js'),
      AGENTSIGHT_TRAJECTORY_TREE_BUILD: join(outputDir, 'utils', 'trajectoryTree.js'),
      AGENTSIGHT_TRAJECTORY_FILTER_BUILD: join(outputDir, 'utils', 'trajectoryTextFilter.js'),
      AGENTSIGHT_ROUND_MODEL_BUILD: join(outputDir, 'utils', 'roundModel.js'),
    },
    stdio: 'inherit',
  });
} finally {
  rmSync(outputDir, { force: true, recursive: true });
}
