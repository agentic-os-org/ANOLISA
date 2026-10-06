// Source-level regression runner: no compilation needed, the test reads the
// page sources directly (same approach as the navigation regression suite).
// The deferred companion additionally transpiles the pages with the
// dashboard's babel toolchain and drives the real loaders with
// manually-resolved responses.
const { execFileSync } = require('node:child_process');

execFileSync('node', [
  '--test',
  'tests/stale-load-regression.test.cjs',
  'tests/stale-load-deferred.test.cjs',
  'tests/atif-import-deferred.test.cjs',
], {
  stdio: 'inherit',
});
