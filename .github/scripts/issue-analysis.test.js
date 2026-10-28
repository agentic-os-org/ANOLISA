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
const { enabledComponents, selectIssue } = require('./issue-analysis-config');
const { discover } = require('./issue-analysis-discovery');

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

test('component variable is an exact comma-separated allowlist with an empty off switch', () => {
  assert.deepEqual(enabledComponents(' sec-core,cosh-ng,sec-core '), ['sec-core', 'cosh-ng']);
  assert.deepEqual(enabledComponents('  '), []);
  for (const value of ['sec', 'component:sec-core', 'sec-core,', 'osbase', '*', 'SEC-CORE']) {
    assert.throws(() => enabledComponents(value), /unknown public component/);
  }
  const issue = { state: 'open', labels: ['component:sec-core'] };
  assert.equal(selectIssue(issue, ['sec-core', 'cosh-ng']).component.id, 'sec-core');
  for (const [change, enabled, reason] of [
    [{}, [], 'analysis_disabled'],
    [{}, ['cosh-ng'], 'component_disabled'],
    [{ labels: [] }, ['sec-core'], 'missing_component'],
    [{ labels: ['component:sec-core-extra'] }, ['sec-core'], 'unsupported_component'],
    [{ labels: ['component:sec-core', 'component:cosh-ng'] }, ['sec-core'], 'ambiguous_component'],
    [{ labels: ['component:sec-core', 'priority:p2'] }, ['sec-core'], 'existing_priority_skipped'],
    [{ state: 'closed' }, ['sec-core'], 'closed_issue'],
    [{ pull_request: {} }, ['sec-core'], 'pull_request'],
  ]) assert.equal(selectIssue({ ...issue, ...change }, enabled).reason, reason);
});

function discoveryHarness() {
  const requests = [];
  const outputs = {};
  const issue = { number: 12, state: 'open', labels: [{ name: 'component:sec-core' }] };
  const github = { rest: {
    repos: {
      get: async () => ({ data: { full_name: 'agentic-os-org/ANOLISA', id: 1195873793 } }),
      getCollaboratorPermissionLevel: async () => ({ data: { permission: 'write' } }),
      getCommit: async (params) => { requests.push(params); return { data: { sha: 'a'.repeat(40) } }; },
    },
    issues: {
      get: async (params) => { requests.push(params); return { data: issue }; },
      listForRepo: async (params) => { requests.push(params); return { data: [issue] }; },
      listLabelsForRepo: 'labels', listComments: 'comments',
    },
  }, paginate: async () => [] };
  const summary = { addHeading: () => summary, addCodeBlock: () => summary, write: async () => {} };
  const args = { github, context: { repo: { owner: 'agentic-os-org', repo: 'ANOLISA' }, actor: 'kongche-jbw' },
    core: { setOutput: (key, value) => { outputs[key] = value; }, summary },
    env: { INPUT_ISSUE_NUMBER: '12', ISSUE_ANALYSIS_COMPONENTS: 'sec-core' } };
  return { args, requests, outputs, issue };
}

test('manual selection skips disabled and mismatched issues before source resolution', async () => {
  const h = discoveryHarness();
  h.args.env.ISSUE_ANALYSIS_COMPONENTS = '';
  assert.deepEqual((await discover(h.args)).selected, []);
  assert.deepEqual(h.requests, []);
  h.args.env.ISSUE_ANALYSIS_COMPONENTS = 'cosh-ng';
  assert.equal((await discover(h.args)).skipped.component_disabled, 1);
  assert.equal(h.requests.length, 1);
  h.args.env.ISSUE_ANALYSIS_COMPONENTS = 'sec-core';
  assert.deepEqual((await discover(h.args)).selected, [12]);
  assert.equal(h.outputs.target_sha, 'a'.repeat(40));
  h.args.env.INPUT_ISSUE_NUMBER = '0';
  await assert.rejects(discover(h.args), /positive integer/);
  h.args.github.rest.repos.getCollaboratorPermissionLevel = async () => ({ data: { permission: 'read' } });
  await assert.rejects(discover(h.args), /repository writer/);
});

test('historical batches paginate, skip priorities and PRs, and respect the candidate limit', async () => {
  const h = discoveryHarness();
  h.args.env.INPUT_MODE = 'batch';
  h.args.env.INPUT_MAX_ISSUES = '2';
  const pages = [];
  h.args.github.rest.issues.listForRepo = async (params) => {
    pages.push(params.page);
    return { data: params.page === 1 ? Array.from({ length: 100 }, (_, number) => ({
      ...h.issue, number: number + 1, labels: ['component:sec-core', 'priority:p3'],
    })) : [{ ...h.issue, number: 101, pull_request: {} }, { ...h.issue, number: 102 },
      { ...h.issue, number: 103 }, { ...h.issue, number: 104 }] };
  };
  assert.deepEqual((await discover(h.args)).selected, [102, 103]);
  assert.deepEqual(pages, [1, 2]);
  for (const invalid of ['0', '26', '1.5', 'NaN']) {
    h.args.env.INPUT_MAX_ISSUES = invalid;
    await assert.rejects(discover(h.args), /between 1 and 25/);
  }
});

test('historical scans terminate after ten full pages and report the scan boundary', async () => {
  const h = discoveryHarness();
  h.args.env.INPUT_MODE = 'batch';
  let pages = 0;
  h.args.github.rest.issues.listForRepo = async () => {
    pages++;
    return { data: Array(100).fill({ ...h.issue, labels: ['component:sec-core', 'priority:p2'] }) };
  };
  const result = await discover(h.args);
  assert.equal(pages, 10);
  assert.deepEqual(result.truncated, ['sec-core']);
  assert.deepEqual(result.selected, []);
});
