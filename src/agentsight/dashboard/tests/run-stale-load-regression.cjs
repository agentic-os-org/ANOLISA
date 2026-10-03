// Source-level regression runner: no compilation needed, the test reads the
// page sources directly (same approach as the navigation regression suite).
const { execFileSync } = require('node:child_process');

execFileSync('node', ['--test', 'tests/stale-load-regression.test.cjs'], {
  stdio: 'inherit',
});
