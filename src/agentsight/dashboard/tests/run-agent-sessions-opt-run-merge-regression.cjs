// Behavioral regression runner for the optimizer's `opt:<target>` rows:
// the companion test transpiles the extracted session merge model
// (src/utils/sessionModel.ts) with the dashboard's babel toolchain and
// drives the real mergeSessions with UUID-targeted optimization runs,
// their subagents, genuine Codex rollout aliases and non-UUID targets.
const { execFileSync } = require('node:child_process');

execFileSync('node', [
  '--test',
  'tests/agent-sessions-opt-run-merge-regression.test.cjs',
], {
  stdio: 'inherit',
});
