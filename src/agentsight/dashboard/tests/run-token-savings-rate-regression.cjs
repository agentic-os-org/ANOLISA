// The test transpiles the page with the dashboard's own babel toolchain, so
// this runner has no extra compilation step.
const { execFileSync } = require('node:child_process');

execFileSync('node', ['--test', 'tests/token-savings-rate-regression.test.cjs'], {
  stdio: 'inherit',
});
