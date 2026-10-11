// Source-level + hooks-driver regression runner for dashboard display
// controls (legend hide, honest sorting, pagination offset clamp).
const { execFileSync } = require('node:child_process');

execFileSync('node', [
  '--test',
  'tests/dashboard-controls-regression.test.cjs',
], {
  stdio: 'inherit',
});
