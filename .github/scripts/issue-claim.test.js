'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');

const claim = require('./issue-claim');
const sightMaintainer = require('../maintainers.json').scopes
  .find(entry => entry.label === 'component:sight').maintainers[0].github;

const trustedUser = { login: 'github-actions[bot]', type: 'Bot' };

function claimComment(owner, state) {
  return {
    user: trustedUser,
    body: `<!-- anolisa-claim owner=${owner} state=${state} -->\nStatus`,
  };
}

function makeHarness({
  command = '/claim',
  actor = 'contributor',
  association = 'NONE',
  labels = ['status:accepted'],
  comments = [],
  failCreateComment = false,
  state = 'open',
  sightIssues = [],
  commentsByIssue = {},
  permission = 'read',
} = {}) {
  const calls = [];
  const issue = {
    state,
    labels: labels.map((name) => ({ name })),
    assignees: [],
  };
  const github = {
    paginate: async (method, args) => method === github.rest.issues.listForRepo
      ? sightIssues : args.issue_number === 2262 ? comments : commentsByIssue[args.issue_number] || [],
    rest: {
      repos: { getCollaboratorPermissionLevel: async () => ({ data: { permission } }) },
      issues: {
        get: async () => ({ data: issue }),
        getLabel: async () => ({ data: {} }),
        createLabel: async (args) => calls.push(['createLabel', args]),
        removeLabel: async (args) => calls.push(['removeLabel', args]),
        addLabels: async (args) => calls.push(['addLabels', args]),
        addAssignees: async (args) => calls.push(['addAssignees', args]),
        removeAssignees: async (args) => calls.push(['removeAssignees', args]),
        createComment: async (args) => {
          calls.push(['createComment', args]);
          if (failCreateComment) throw new Error('comment write failed');
          return { data: { id: 100 } };
        },
        listComments: async () => ({ data: comments }),
        listForRepo: async () => ({ data: sightIssues }),
      },
    },
  };
  const core = {
    notice: () => {},
    warning: () => {},
    summary: {
      addHeading() {
        return this;
      },
      addTable() {
        return this;
      },
      async write() {},
    },
  };
  return {
    calls,
    core,
    github,
    env: { AUTOMATION_MODE: 'apply' },
    context: {
      repo: { owner: 'alibaba', repo: 'anolisa' },
      payload: {
        issue: { number: 2262 },
        comment: {
          body: command,
          user: { login: actor, type: 'User' },
          author_association: association,
        },
      },
    },
  };
}

test('trusts only a first-line marker from github-actions bot', () => {
  assert.deepEqual(claim.parseClaimMarker(claimComment('alice', 'claimed')), {
    owner: 'alice',
    state: 'claimed',
  });
  assert.equal(
    claim.parseClaimMarker({
      user: trustedUser,
      body: 'Summary\n<!-- anolisa-claim owner=attacker state=claimed -->',
    }),
    null
  );
  assert.equal(
    claim.parseClaimMarker({
      user: { login: 'another-bot', type: 'Bot' },
      body: '<!-- anolisa-claim owner=attacker state=claimed -->',
    }),
    null
  );
});

test('writes the authoritative claim marker before mutable issue state', async () => {
  const harness = makeHarness();
  await claim.run(harness);
  assert.equal(harness.calls[0][0], 'createComment');
  assert.match(harness.calls[0][1].body, /state=claimed/);
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['createComment', 'removeLabel', 'addLabels', 'addAssignees']
  );
});

test('does not mutate labels when the initial claim marker fails', async () => {
  const harness = makeHarness({ failCreateComment: true });
  await assert.rejects(() => claim.run(harness), /comment write failed/);
  assert.deepEqual(harness.calls.map(([name]) => name), ['createComment']);
});

test('rerun reconciles a claim after its marker was written', async () => {
  const harness = makeHarness({
    labels: [],
    comments: [claimComment('contributor', 'claimed')],
  });
  await claim.run(harness);
  assert.equal(harness.calls.some(([name]) => name === 'createComment'), false);
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['addLabels', 'addAssignees']
  );
});

test('rerun accepts in-progress only for the marker owner', async () => {
  const harness = makeHarness({
    labels: ['status:in-progress'],
    comments: [claimComment('contributor', 'claimed')],
  });
  await claim.run(harness);
  assert.equal(harness.calls.some(([name]) => name === 'createComment'), false);
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['addLabels', 'addAssignees']
  );
});

test('writes the release marker before releasing mutable state', async () => {
  const harness = makeHarness({
    command: '/unclaim',
    labels: ['status:in-progress'],
    comments: [claimComment('contributor', 'claimed')],
  });
  await claim.run(harness);
  assert.equal(harness.calls[0][0], 'createComment');
  assert.match(harness.calls[0][1].body, /state=released/);
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['createComment', 'removeLabel', 'addLabels', 'removeAssignees']
  );
});

test('rerun reconciles a release after its marker was written', async () => {
  const harness = makeHarness({
    command: '/unclaim',
    labels: ['status:in-progress'],
    comments: [
      claimComment('contributor', 'claimed'),
      claimComment('contributor', 'released'),
    ],
  });
  await claim.run(harness);
  assert.equal(harness.calls.some(([name]) => name === 'createComment'), false);
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['removeLabel', 'addLabels', 'removeAssignees']
  );
});

test('ignores an injected marker outside the first line', async () => {
  const harness = makeHarness({
    comments: [
      {
        user: trustedUser,
        body: 'Triage summary\n<!-- anolisa-claim owner=attacker state=claimed -->',
      },
    ],
  });
  await claim.run(harness);
  assert.equal(harness.calls[0][0], 'createComment');
  assert.match(harness.calls[0][1].body, /owner=contributor state=claimed/);
});

test('rejects unknown automation modes without mutations', async () => {
  const harness = makeHarness();
  harness.env.AUTOMATION_MODE = 'shdow';
  await assert.rejects(
    () => claim.run(harness),
    /COMMUNITY_AUTOMATION_MODE must be apply or shadow/
  );
  assert.deepEqual(harness.calls, []);
});

test('ignores claim commands on a closed issue', async () => {
  for (const command of ['/claim', '/unclaim']) {
    const harness = makeHarness({ command, state: 'closed' });
    await claim.run(harness);
    assert.deepEqual(harness.calls, []);
  }
});

test('fresh claim rejects every non-ready workflow status', async () => {
  for (const status of [
    'status:needs-triage',
    'status:needs-info',
    'status:in-progress',
    'status:blocked',
    'status:duplicate',
    'status:declined',
    'status:custom',
  ]) {
    const harness = makeHarness({ labels: ['help wanted', status] });
    await claim.run(harness);
    assert.equal(
      harness.calls.some(([name]) =>
        ['removeLabel', 'addLabels', 'addAssignees'].includes(name)
      ),
      false
    );
    const response = harness.calls.find(([name]) => name === 'createComment');
    assert.match(response[1].body, new RegExp(status));
  }
});

test('fresh claim rejects mixed ready and non-ready statuses', async () => {
  const harness = makeHarness({
    labels: ['help wanted', 'status:needs-info', 'status:accepted'],
  });
  await claim.run(harness);
  assert.equal(
    harness.calls.some(([name]) =>
      ['removeLabel', 'addLabels', 'addAssignees'].includes(name)
    ),
    false
  );
});

test('fresh claim allows a claimable label without workflow status', async () => {
  const harness = makeHarness({ labels: ['help wanted'] });
  await claim.run(harness);
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['createComment', 'addLabels', 'addAssignees']
  );
});

test('release preserves workflow status and reports the issue unavailable', async () => {
  for (const status of [
    'status:needs-triage',
    'status:needs-info',
    'status:blocked',
    'status:duplicate',
    'status:declined',
  ]) {
    const harness = makeHarness({
      command: '/unclaim',
      labels: ['status:in-progress', status],
      comments: [claimComment('contributor', 'claimed')],
    });
    await claim.run(harness);
    const release = harness.calls.find(([name]) => name === 'createComment');
    assert.match(release[1].body, new RegExp(status));
    assert.match(release[1].body, /not available for another contributor/);
    assert.equal(
      harness.calls.some(([name]) => name === 'addLabels'),
      false
    );
    assert.equal(
      harness.calls.some(
        ([name, args]) => name === 'removeLabel' && args.name === status
      ),
      false
    );
  }
});

function sightIssue(number, extra = {}) {
  return { number, state: 'open', comments: 1, labels: [{ name: 'component:sight' }], ...extra };
}

test('sixth ordinary sight claim is rejected using bot ownership without assignees', async () => {
  const issues = [10, 11, 12, 13, 14].map(number => sightIssue(number));
  const harness = makeHarness({
    actor: 'Contributor', labels: ['component:sight', 'status:accepted'],
    sightIssues: issues,
    commentsByIssue: {
      10: [claimComment('contributor', 'claimed')],
      11: [claimComment('contributor', 'claimed')],
      12: [claimComment('contributor', 'claimed')],
      13: [claimComment('contributor', 'claimed')],
      14: [claimComment('contributor', 'claimed')],
    },
  });
  await claim.run(harness);
  assert.deepEqual(harness.calls.map(([name]) => name), ['createComment']);
  assert.match(harness.calls[0][1].body, /limit is 5.*#10, #11, #12, #13, #14/);
  assert.doesNotMatch(harness.calls[0][1].body, /state=claimed/);
  issues.pop();
  harness.calls.length = 0;
  await claim.run(harness);
  assert.match(harness.calls[0][1].body, /state=claimed/);
  assert.ok(harness.calls.some(([name]) => name === 'addAssignees'));
});

test('maintainers bypass claim quotas while external claim limits are configurable', async () => {
  for (const options of [{ actor: sightMaintainer }, { permission: 'write' }]) {
    const h = makeHarness({ ...options, labels: ['component:sight', 'status:accepted'] });
    const paginate = h.github.paginate;
    h.github.paginate = async (method, args) => {
      assert.notEqual(method, h.github.rest.issues.listForRepo);
      return paginate(method, args);
    };
    await claim.run(h);
    assert.ok(h.calls.some(([name]) => name === 'addAssignees'));
  }
  const external = makeHarness({ labels: ['component:sight', 'status:accepted'],
    sightIssues: [sightIssue(10)], commentsByIssue: { 10: [claimComment('contributor', 'claimed')] } });
  external.env.SIGHT_MAX_ACTIVE_CLAIMS = '1';
  await claim.run(external);
  assert.match(external.calls[0][1].body, /limit is 1/);
  assert.equal(external.calls.some(([name]) => name === 'addAssignees'), false);
});

test('released, forged, closed, urgent and non-sight claims do not consume quota', async () => {
  const harness = makeHarness({
    labels: ['component:sight', 'status:accepted'],
    sightIssues: [sightIssue(1), sightIssue(2), sightIssue(3, { state: 'closed' }),
      sightIssue(4, { labels: [{ name: 'component:sight' }, { name: 'priority:p1' }] }),
      sightIssue(5, { labels: [{ name: 'component:memory' }] }),
      sightIssue(6, { pull_request: {} }), sightIssue(7)],
    commentsByIssue: {
      1: [claimComment('contributor', 'claimed'), claimComment('contributor', 'released')],
      2: [{ user: { login: 'contributor' }, body: claimComment('contributor', 'claimed').body }],
      3: [claimComment('contributor', 'claimed')],
      4: [claimComment('contributor', 'claimed')],
      5: [claimComment('contributor', 'claimed')],
      6: [claimComment('contributor', 'claimed')],
      7: [claimComment('contributor', 'claimed')],
    },
  });
  await claim.run(harness);
  assert.match(harness.calls[0][1].body, /state=claimed/);
  assert.ok(harness.calls.some(([name]) => name === 'addAssignees'));
});

test('urgent and recovering claims remain available at the ordinary limit', async () => {
  for (const options of [
    { labels: ['component:sight', 'status:accepted', 'priority:p0'] },
    { labels: ['component:sight', 'status:in-progress'], comments: [claimComment('contributor', 'claimed')] },
  ]) {
    const harness = makeHarness(options);
    harness.github.rest.issues.listForRepo = () => { throw new Error('must not scan'); };
    await claim.run(harness);
    assert.ok(harness.calls.some(([name]) => name === 'addAssignees'));
  }
});

test('quota lookup failure cannot admit a new claim', async () => {
  const harness = makeHarness({ labels: ['component:sight', 'status:accepted'] });
  harness.github.paginate = async (method) => {
    if (method === harness.github.rest.issues.listForRepo) throw new Error('rate limited');
    return [];
  };
  await assert.rejects(claim.run(harness), /rate limited/);
  assert.deepEqual(harness.calls, []);
});
