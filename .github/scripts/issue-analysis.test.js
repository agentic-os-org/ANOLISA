'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const analysis = require('./issue-analysis');
const metadata = require('../components.json');
const profiles = require('../issue-analysis/components.json');

test('every public component has real source entrypoints and a scoped policy', () => {
  const root = path.resolve(__dirname, '../..');
  for (const component of metadata.components.filter((item) => item.status !== 'internal')) {
    const scope = analysis.componentScope(component);
    assert.ok(scope.architecture && scope.notes && scope.entrypoints.length);
    assert.match(scope.policy_sha256, /^[a-f0-9]{64}$/);
    for (const entry of scope.entrypoints) {
      assert.ok(component.path_prefixes.some((prefix) => entry.startsWith(prefix)));
      assert.ok(fs.statSync(path.join(root, entry)).isFile(), entry);
    }
  }
  assert.deepEqual(Object.keys(profiles).sort(), metadata.components
    .filter((item) => item.status !== 'internal').map((item) => item.id).sort());
});

test('selects exactly one public component label and rejects ambiguous scopes', () => {
  for (const component of metadata.components.filter((item) => item.status !== 'internal')) {
    assert.equal(analysis.componentForIssue({ labels: [component.label, 'bug'] }).component.id, component.id);
  }
  assert.equal(analysis.componentForIssue({ labels: [] }).reason, 'missing_component');
  assert.equal(analysis.componentForIssue({ labels: ['component:sec-core', 'component:cosh-ng'] }).reason, 'ambiguous_component');
  assert.equal(analysis.componentForIssue({ labels: ['component:osbase'] }).reason, 'unsupported_component');
});

test('stages a non-Rust component without requiring SecCore or Cargo entrypoints', (t) => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'issue-analysis-'));
  t.after(() => fs.rmSync(temporary, { recursive: true, force: true }));
  const checkout = path.join(temporary, 'checkout');
  const root = path.join(temporary, 'run');
  const entry = profiles.cosh.entrypoints[0];
  fs.mkdirSync(path.dirname(path.join(checkout, entry)), { recursive: true });
  fs.writeFileSync(path.join(checkout, entry), '{"name":"fixture"}\n');
  const git = (...args) => execFileSync('git', ['-C', checkout, ...args], { encoding: 'utf8', stdio: 'pipe' }).trim();
  git('init', '--quiet');
  git('add', '.');
  git('-c', 'commit.gpgsign=false', '-c', 'user.name=kongche-jbw',
    '-c', 'user.email=kongche.jbw@alibaba-inc.com', 'commit', '--quiet', '-m', 'fixture');
  fs.mkdirSync(path.join(root, 'input'), { recursive: true });
  fs.writeFileSync(path.join(root, 'input/snapshot.json'), JSON.stringify({
    target: { source_commit_sha: git('rev-parse', 'HEAD') }, scope: profiles.cosh,
  }));
  analysis.stageSource(checkout, root);
  assert.equal(fs.readFileSync(path.join(root, 'source', entry), 'utf8'), '{"name":"fixture"}\n');
  fs.rmSync(path.join(root, 'source'), { recursive: true });
  fs.writeFileSync(path.join(root, 'input/snapshot.json'), JSON.stringify({
    target: { source_commit_sha: git('rev-parse', 'HEAD') }, scope: profiles['sec-core'],
  }));
  assert.throws(() => analysis.stageSource(checkout, root), /Missing component entrypoint/);
});
