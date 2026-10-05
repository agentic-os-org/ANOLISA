// Behavioral regression runner for the merged-session subagent badge: the
// companion test transpiles the extracted session merge model
// (src/utils/sessionModel.ts) with the dashboard's babel toolchain and
// drives the real mergeSessions with aliased Codex parents, exact-ID
// parents, source-order permutations and orphan children.
const { execFileSync } = require('node:child_process');

execFileSync('node', [
  '--test',
  'tests/agent-sessions-subagent-count-regression.test.cjs',
], {
  stdio: 'inherit',
});
