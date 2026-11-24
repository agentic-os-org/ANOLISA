// Source-level + hooks-driver regression runner for dashboard display
// controls (legend hide, honest sorting, pagination offset clamp) and
// token-savings selection/filter-range regressions run in CI.
const { execFileSync } = require('node:child_process');

for (const suite of [
  'tests/dashboard-controls-regression.test.cjs',
  'tests/token-savings-selection-regression.test.cjs',
]) {
  execFileSync('node', ['--test', suite], {
    stdio: 'inherit',
  });
}
