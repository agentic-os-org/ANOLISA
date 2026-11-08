const { execFileSync } = require('node:child_process');

execFileSync('node', ['--test', 'tests/workspace-snapshots-deferred.test.cjs'], {
  stdio: 'inherit',
});
