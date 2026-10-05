// Behavioral regression runner for the token-savings page's selected-session
// export: transpiles the page with the dashboard's babel toolchain and drives
// the real selection / export interactions with manually-resolved responses
// (same approach as the token-savings failed-query regression suite).
const { execFileSync } = require('node:child_process');

execFileSync('node', [
  '--test',
  'tests/token-savings-selection-regression.test.cjs',
], {
  stdio: 'inherit',
});
