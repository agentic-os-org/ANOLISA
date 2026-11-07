'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const policy = require('./sight-pr-policy');
const sightMaintainer = require('../maintainers.json').scopes
  .find(entry => entry.label === 'component:sight').maintainers[0].github;
const head = 'a'.repeat(40);
const body = 'Regression evidence: reproducer fails on parent and passes with the fix';

test('only the regression explanation is required; no manually maintained SHA or CI fields', () => {
  assert.deepEqual(policy.validationErrors(body), []);
  assert.deepEqual(policy.validationErrors('Regression evidence: not applicable: documentation-only change'), []);
  assert.deepEqual(policy.validationErrors(`${body}\nValidation head: obsolete-sha\nValidation commands: old log\nUnverified: CI pending`), []);
  for (const text of ['', 'Regression evidence: <regression case>',
    'Regression evidence: TBD', 'Regression evidence: n/a',
    `<!--\n${body}\n-->`, `\`\`\`markdown\n${body}\n\`\`\``, `${body}\n${body}`]) {
    assert.ok(policy.validationErrors(text).length, text);
  }
});

function pr(number, extra = {}) {
  const sha = number.toString(16).padStart(40, '0');
  return { number, state: 'open', draft: false, labels: [], user: { login: 'contributor' },
    head: { sha }, base: { ref: 'main' }, changed_files: 1,
    body, ...extra };
}

function harness(current = pr(3), others = [pr(1), pr(2)], files = {}) {
  const failures = [], messages = [], writes = [], statuses = new Map();
  const pulls = [current, ...others];
  const github = {
    rest: { repos: {
      getCollaboratorPermissionLevel: async () => ({ data: { permission: 'read' } }),
      createCommitStatus: async args => { writes.push(args); statuses.set(args.sha, args.state); },
    }, pulls: {
      get: async ({ pull_number: number }) => ({ data: structuredClone(pulls.find(p => p.number === number)) }),
      list: async () => {}, listFiles: async () => {},
    } },
    paginate: async (method, args) => method === github.rest.pulls.list ?
      structuredClone(pulls.filter(pr => pr.state === 'open')) :
      files[args.pull_number] || [{ filename: 'src/agentsight/src/local.rs' }],
  };
  return { github, failures, messages, env: {}, pulls, writes, statuses,
    context: { repo: { owner: 'org', repo: 'repo' }, runId: 123,
      payload: { pull_request: structuredClone(current) } },
    core: { notice: text => messages.push(text), setFailed: text => failures.push(text),
      summary: { addHeading() { return this; }, addRaw(text) { messages.push(text); return this; }, async write() {} } },
  };
}

function status(h, number) {
  return h.statuses.get(h.pulls.find(pr => pr.number === number).head.sha);
}

async function event(h, number, action) {
  h.context.payload = { action, pull_request: structuredClone(h.pulls.find(pr => pr.number === number)) };
  h.failures.length = 0;
  await policy.run(h);
}

test('refresh publishes failure on the sixth PR and success on the oldest five heads', async () => {
  const h = harness(pr(6), [1, 2, 3, 4, 5].map(n => pr(n)));
  await policy.run(h);
  for (const n of [1, 2, 3, 4, 5]) assert.equal(status(h, n), 'success');
  assert.equal(status(h, 6), 'failure');
  assert.match(h.messages.join('\n'), /limit is 5.*#1, #2, #3, #4, #5/);
  assert.ok(h.writes.every(w => w.context === policy.STATUS_CONTEXT &&
    w.target_url === 'https://github.com/org/repo/actions/runs/123'));
});

test('a blocked sibling does not fail the admitted event PR workflow', async () => {
  const h = harness(pr(1), [2, 3, 4, 5, 6].map(n => pr(n)));
  await policy.run(h);
  assert.deepEqual(h.failures, []);
  assert.equal(status(h, 1), 'success');
  assert.equal(status(h, 6), 'failure');
});

test('six Draft PRs becoming Ready in reverse order revoke the sixth PR green result', async () => {
  const h = harness(pr(6, { draft: true }), [1, 2, 3, 4, 5].map(n => pr(n, { draft: true })));
  for (const n of [6, 5, 4, 3, 2, 1]) {
    h.pulls.find(pr => pr.number === n).draft = false;
    await event(h, n, 'ready_for_review');
    assert.equal(status(h, 6), n === 1 ? 'failure' : 'success');
  }
  for (const n of [1, 2, 3, 4, 5]) assert.equal(status(h, n), 'success');
});

test('Draft, close, merge, reopen and priority changes refresh sibling results', async () => {
  const h = harness(pr(6), [1, 2, 3, 4, 5].map(n => pr(n)));
  const first = h.pulls.find(pr => pr.number === 1);
  await policy.run(h);
  assert.equal(status(h, 6), 'failure');
  for (const [action, update] of [
    ['converted_to_draft', { draft: true }], ['ready_for_review', { draft: false }],
    ['closed', { state: 'closed' }], ['reopened', { state: 'open' }],
    ['closed', { state: 'closed', merged: true }], ['reopened', { state: 'open', merged: false }],
    ['labeled', { labels: [{ name: 'priority:p1' }] }], ['unlabeled', { labels: [] }],
    ['edited', { base: { ref: 'unprotected-topic' } }], ['edited', { base: { ref: 'release/1.0' } }],
  ]) {
    Object.assign(first, update);
    await event(h, 1, action);
    assert.equal(status(h, 6), ['converted_to_draft', 'closed', 'labeled'].includes(action) ||
      first.base.ref === 'unprotected-topic' ? 'success' : 'failure', action);
  }
});

test('maintainers and repository writers bypass quotas but still need evidence on every PR', async () => {
  for (const login of [sightMaintainer, 'contributor']) {
    const h = harness(pr(9, { user: { login }, body: '' }), []);
    h.github.rest.repos.getCollaboratorPermissionLevel = async ({ username }) => {
      assert.equal(username, login);
      return { data: { permission: 'write' } };
    };
    await policy.run(h);
    assert.equal(status(h, 9), 'failure');
    assert.match(h.messages.join('\n'), /Regression evidence/);
    assert.doesNotMatch(h.messages.join('\n'), /limit is/);
  }
});

test('quota uses author permissions rather than the event actor and supports overrides', async () => {
  const h = harness();
  h.context.actor = 'maintainer-editing-the-PR';
  h.env.SIGHT_MAX_READY_PRS = '2';
  h.github.rest.repos.getCollaboratorPermissionLevel = async ({ username }) => {
    assert.equal(username, 'contributor');
    return { data: { permission: 'read' } };
  };
  await policy.run(h);
  assert.equal(status(h, 3), 'failure');
  assert.match(h.messages.join('\n'), /limit is 2.*#1, #2/);
});

test('Draft, other authors, other components and urgent PRs do not consume slots', async () => {
  const h = harness(pr(9), [pr(1, { draft: true }), pr(2, { user: { login: 'other' } }),
    pr(3), pr(4, { labels: [{ name: 'priority:p1' }] })],
  { 3: [{ filename: 'src/agent-memory/src/lib.rs' }] });
  h.env.SIGHT_MAX_READY_PRS = '1';
  await policy.run(h);
  assert.equal(status(h, 9), 'success');
  assert.equal(status(h, 2), undefined);
});

test('renamed sight files cannot bypass evidence', async () => {
  const h = harness(pr(3, { body: '' }), [], {
    3: [{ filename: 'src/other/file.rs', previous_filename: 'src/agentsight/file.rs' }],
  });
  await policy.run(h);
  assert.equal(status(h, 3), 'failure');
});

test('non-sight and Draft PRs do not require sight evidence', async () => {
  for (const h of [harness(pr(3, { body: '', draft: true }), []),
    harness(pr(3, { body: '' }), [], { 3: [{ filename: 'README.md' }] })]) {
    await policy.run(h);
    assert.equal(status(h, 3), 'success');
  }
});

test('urgent PRs bypass quota but still need current-head evidence', async () => {
  const h = harness(pr(3, { labels: [{ name: 'priority:p0' }], body: '' }), []);
  await policy.run(h);
  assert.match(h.messages.join('\n'), /Regression evidence/);
  assert.doesNotMatch(h.messages.join('\n'), /external contributor limit/);
});

test('queued events evaluate live heads and evidence, never the stale event head', async () => {
  const h = harness(pr(3), []);
  const old = h.pulls[0].head.sha;
  h.pulls[0].head.sha = 'b'.repeat(40);
  const unchangedBody = h.pulls[0].body;
  await policy.run(h);
  assert.equal(status(h, 3), 'success');
  assert.equal(h.statuses.get(old), 'pending');
  assert.equal(h.pulls[0].body, unchangedBody);
  assert.ok(h.messages.some(message => message.includes('b'.repeat(40)) && message.includes('[CI checks](https://github.com/org/repo/pull/3/checks)')));
});

test('a new push uses the live SHA without changing the PR description or copying CI output', async () => {
  const h = harness(pr(3), []);
  await policy.run(h);
  const previous = h.pulls[0].head.sha;
  h.pulls[0].head.sha = 'c'.repeat(40);
  await event(h, 3, 'synchronize');
  assert.equal(status(h, 3), 'success');
  assert.equal(h.pulls[0].body, body);
  assert.ok(h.messages.some(text => text.includes('c'.repeat(40)) && text.includes('separate required CI checks')));
  assert.notEqual(h.pulls[0].head.sha, previous);
});

test('admission rejection never fails the required execution workflow and recovers without a push', async () => {
  const h = harness(pr(6), [1, 2, 3, 4, 5].map(n => pr(n)));
  const sameHead = h.pulls[0].head.sha;
  await policy.run(h);
  assert.equal(status(h, 6), 'failure');
  assert.deepEqual(h.failures, []);
  h.pulls.find(pr => pr.number === 1).state = 'closed';
  await event(h, 1, 'closed');
  assert.equal(status(h, 6), 'success');
  assert.equal(h.pulls[0].head.sha, sameHead);
  assert.deepEqual(h.failures, []);
  h.pulls[0].body = '';
  await event(h, 6, 'edited');
  assert.equal(status(h, 6), 'failure');
  assert.deepEqual(h.failures, []);
  h.pulls[0].body = body;
  await event(h, 6, 'edited');
  assert.equal(status(h, 6), 'success');
  assert.deepEqual(h.failures, []);
});

test('different authors sharing a SHA cannot overwrite a rejected PR through a Draft', async () => {
  const shared = 'd'.repeat(40);
  const h = harness(pr(6, { head: { sha: shared } }), [1, 2, 3, 4, 5].map(n => pr(n, { draft: true })));
  await policy.run(h);
  assert.equal(status(h, 6), 'success');
  for (const pr of h.pulls) pr.draft = false;
  await event(h, 1, 'ready_for_review');
  assert.equal(status(h, 6), 'failure');
  h.pulls.push(pr(7, { user: { login: 'bob' }, head: { sha: shared }, draft: true, body: '' }));
  await event(h, 7, 'opened');
  assert.equal(status(h, 6), 'failure');
  assert.equal(status(h, 7), 'failure');
  h.pulls.find(pr => pr.number === 1).state = 'closed';
  await event(h, 1, 'closed');
  assert.equal(status(h, 6), 'success');
});

test('shared-head closure calculates each author quota and refreshes transitively related authors', async () => {
  const shared = 'd'.repeat(40), otherShared = 'e'.repeat(40);
  const h = harness(pr(6, { head: { sha: shared } }), [1, 2, 3, 4, 5].map(n => pr(n)));
  h.pulls.push(pr(7, { user: { login: 'bob' }, head: { sha: shared }, draft: true }));
  h.pulls.push(...[8, 9, 10, 11, 12, 13].map(n => pr(n, { user: { login: 'bob' },
    ...(n === 13 ? { head: { sha: otherShared } } : {}) })));
  h.pulls.push(pr(14, { user: { login: 'carol' }, head: { sha: otherShared }, draft: true }));
  await event(h, 14, 'opened');
  assert.equal(status(h, 6), 'failure');
  assert.equal(status(h, 13), 'failure');
  for (const n of [1, 5, 8, 12]) assert.equal(status(h, n), 'success');
});

test('a different author sharing the head during publication invalidates the snapshot', async () => {
  const h = harness(pr(6), [1, 2, 3, 4, 5].map(n => pr(n)));
  const create = h.github.rest.repos.createCommitStatus;
  let changed = false;
  h.github.rest.repos.createCommitStatus = async args => {
    await create(args);
    if (!changed && args.state === 'failure') {
      changed = true;
      h.pulls.push(pr(7, { user: { login: 'bob' }, head: { sha: h.pulls[0].head.sha }, draft: true }));
    }
  };
  await policy.run(h);
  assert.equal(status(h, 6), 'failure');
  assert.ok(h.writes.some(write => write.state === 'pending' && /changed/.test(write.description)));
});

test('synchronize refreshes other authors still using the previous head', async () => {
  const previous = 'd'.repeat(40);
  const h = harness(pr(6, { head: { sha: previous }, body: '' }), [pr(7, {
    user: { login: 'bob' }, head: { sha: previous }, draft: true,
  })]);
  await policy.run(h);
  assert.equal(status(h, 7), 'failure');
  h.pulls[0].head.sha = 'e'.repeat(40);
  h.pulls[0].body = body;
  h.context.payload.before = previous;
  await policy.run(h);
  assert.equal(status(h, 7), 'success');
  assert.equal(status(h, 6), 'success');
});

test('API truncation, permission outages and invalid limits fail closed on every known head', async () => {
  for (const mode of ['files', 'permissions', 'limit']) {
    const h = harness();
    if (mode === 'files') h.pulls[0].changed_files = 2;
    if (mode === 'permissions') h.github.rest.repos.getCollaboratorPermissionLevel = async () => { throw new Error('outage'); };
    if (mode === 'limit') h.env.SIGHT_MAX_READY_PRS = '0';
    await assert.rejects(policy.run(h));
    assert.ok(h.pulls.every(pr => status(h, pr.number) === 'error'));
  }
});

test('a sibling changing during publication invalidates stale results and retries live state', async () => {
  const h = harness(pr(6), [1, 2, 3, 4, 5].map(n => pr(n, { draft: true })));
  const create = h.github.rest.repos.createCommitStatus;
  let changed = false;
  h.github.rest.repos.createCommitStatus = async args => {
    await create(args);
    if (!changed && args.state === 'success') {
      changed = true;
      for (const pr of h.pulls) pr.draft = false;
    }
  };
  await policy.run(h);
  assert.equal(status(h, 6), 'failure');
  assert.ok(h.writes.some(w => w.state === 'pending' && /changed/.test(w.description)));
});

test('body and priority changes during calculation are recalculated before publishing', async () => {
  for (const mutate of [pr => { pr.body = ''; },
    pr => { pr.labels = []; }]) {
    const h = harness(pr(6, { labels: [{ name: 'priority:p1' }] }), [1, 2, 3, 4, 5].map(n => pr(n)));
    const paginate = h.github.paginate;
    let changed = false;
    h.github.paginate = async (method, args) => {
      if (!changed && method === h.github.rest.pulls.listFiles) { changed = true; mutate(h.pulls[0]); }
      return paginate(method, args);
    };
    await policy.run(h);
    assert.equal(status(h, 6), 'failure');
  }
});

test('continuous edits exhaust retries with error, never stale green', async () => {
  const h = harness(pr(3), []);
  const paginate = h.github.paginate;
  h.github.paginate = async (method, args) => {
    if (method === h.github.rest.pulls.listFiles) h.pulls[0].body += ' ';
    return paginate(method, args);
  };
  await assert.rejects(policy.run(h), /kept changing/);
  assert.equal(status(h, 3), 'error');
});

test('a detail API failure revokes previous sibling greens before failing', async () => {
  const h = harness();
  await policy.run(h);
  h.github.rest.pulls.get = async () => { throw new Error('detail unavailable'); };
  await assert.rejects(policy.run(h), /detail unavailable/);
  assert.ok(h.pulls.every(pr => status(h, pr.number) === 'error'));
});

test('status write failures still invalidate other heads', async () => {
  const h = harness();
  const create = h.github.rest.repos.createCommitStatus;
  h.github.rest.repos.createCommitStatus = async args => {
    if (args.sha === h.pulls[0].head.sha) throw new Error('status unavailable');
    return create(args);
  };
  await assert.rejects(policy.run(h), /Cannot invalidate/);
  assert.equal(status(h, 1), 'error');
  assert.equal(status(h, 2), 'error');
});

test('PRs sharing a head fail conservatively if either fails evidence', async () => {
  const h = harness(pr(3, { head: { sha: head }, body }), [pr(1, { head: { sha: head }, body: '' })]);
  await policy.run(h);
  assert.equal(h.statuses.get(head), 'failure');
});

test('the actual workflow entrypoint runs trusted policy despite a fork rewriting both files', async () => {
  const os = require('node:os');
  const { createRequire } = require('node:module');
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), 'sight-trusted-policy-'));
  try {
    const trusted = path.join(workspace, 'trusted');
    const fork = path.join(workspace, 'fork');
    for (const root of [trusted, fork]) fs.mkdirSync(path.join(root, '.github/scripts'), { recursive: true });
    for (const name of ['sight-pr-policy.js', 'sight-work-limits.js']) {
      fs.copyFileSync(path.join(__dirname, name), path.join(trusted, '.github/scripts', name));
    }
    fs.copyFileSync(path.join(__dirname, '../maintainers.json'), path.join(trusted, '.github/maintainers.json'));
    fs.writeFileSync(path.join(fork, '.github/scripts/sight-pr-policy.js'),
      'exports.run = async () => { throw new Error("UNTRUSTED PR CODE EXECUTED"); };');
    fs.mkdirSync(path.join(fork, '.github/workflows'), { recursive: true });
    fs.writeFileSync(path.join(fork, '.github/workflows/sight-submission-policy.yml'),
      'on: pull_request\njobs:\n  policy:\n    name: sight/submission-policy\n    steps:\n      - run: exit 0\n');
    const source = fs.readFileSync(path.join(__dirname, '../workflows/sight-submission-policy.yml'), 'utf8');
    const script = source.split('          script: |\n')[1].split('\n').map(line => line.replace(/^            /, '')).join('\n');
    const h = harness(pr(3, { body: '' }), []);
    h.context.payload.pull_request.head.repo = { full_name: 'attacker/fork', localPath: fork };
    const AsyncFunction = Object.getPrototypeOf(async function () {}).constructor;
    await new AsyncFunction('process', 'require', 'github', 'context', 'core', script)(
      { env: { GITHUB_WORKSPACE: trusted } }, createRequire(__filename), h.github, h.context, h.core);
    assert.equal(status(h, 3), 'failure');
    assert.match(h.messages.join('\n'), /Regression evidence/);
    assert.ok(h.writes.every(write => write.context === policy.STATUS_CONTEXT));
  } finally {
    fs.rmSync(workspace, { recursive: true, force: true });
  }
});

test('workflow has an immutable entrypoint, minimal status permission and queued repository refreshes', () => {
  const workflow = name => fs.readFileSync(path.join(__dirname, '../workflows', name), 'utf8');
  const submission = workflow('sight-submission-policy.yml');
  assert.match(submission, /pull_request_target:/);
  assert.match(submission, /types: \[opened, synchronize, reopened, closed, edited, ready_for_review, converted_to_draft, labeled, unlabeled\]/);
  assert.doesNotMatch(submission, /\n  pull_request:|branches:|cancel-in-progress: true|head\.sha|head\.ref|refs\/pull|secrets\.|Bootstrap|existsSync/);
  assert.match(submission, /ref: \$\{\{ github\.sha \}\}/);
  assert.match(submission, /persist-credentials: false/);
  assert.match(submission, /sparse-checkout: \|\n\s+\.github\/scripts\/sight-pr-policy\.js\n\s+\.github\/scripts\/sight-work-limits\.js\n\s+\.github\/maintainers\.json/);
  assert.match(submission, /contents: read\n  pull-requests: read\n  statuses: write/);
  assert.doesNotMatch(submission, /pull-requests: write|contents: write|checks: write/);
  assert.match(submission, /group: sight-submission-policy\n/);
  assert.match(submission, /cancel-in-progress: false\n  queue: max/);
  assert.match(submission, /await policy\.run\(\{ github, context, core \}\)/);
  const claim = workflow('issue-claim.yml');
  assert.match(claim, /group: issue-claim-\$\{\{ github\.event\.issue\.number \}\}/);
  assert.match(claim, /group: issue-claim-author-\$\{\{ github\.event\.comment\.user\.login \}\}/);
  assert.equal((claim.match(/queue: max/g) || []).length, 2);
});

test('the PR template delegates SHA and build/test evidence to GitHub CI', () => {
  const template = fs.readFileSync(path.join(__dirname, '../pull_request_template.md'), 'utf8');
  assert.doesNotMatch(template, /Validation head:|Validation environment:|Validation commands:|Unverified:/);
  assert.match(template, /Regression evidence:/);
  assert.match(template, /Test agentsight/);
});
