'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');

const triage = require('./issue-triage');

const metadata = {
  defaults: {
    issue_triagers: ['fallback'],
  },
  components: [
    {
      id: 'sight',
      display_name: 'AgentSight',
      label: 'component:sight',
      issue_triagers: ['chengshuyi', 'jfeng18'],
      status: 'active',
    },
    {
      id: 'osbase',
      display_name: 'OS Base',
      label: 'component:osbase',
      issue_triagers: ['casparant'],
      status: 'internal',
    },
  ],
};

function inputEnv(overrides = {}) {
  return {
    INPUT_ISSUE_NUMBER: '2262',
    INPUT_COMPONENT: 'sight',
    INPUT_CONFIDENCE: '0.96',
    INPUT_SUMMARY: 'The report references AgentSight symbol resolution.',
    INPUT_EVIDENCE: 'src/agentsight and the [sight] title prefix',
    INPUT_DECISION_SOURCE: 'classifier',
    INPUT_DECISION_ID: 'anolisa-2262-v1',
    INPUT_APPLY: 'true',
    AUTOMATION_MODE: 'apply',
    MIN_CONFIDENCE: '0.80',
    ISSUE_TRIAGE_POLICY: JSON.stringify({
      auto_assign_authors: ['trusted-reporter'],
    }),
    ...overrides,
  };
}

function makeHarness({
  author = 'external-user',
  labels = [],
  comments = [],
  state = 'open',
  assignees = [],
  acceptedAssignees,
} = {}) {
  const calls = [];
  const issue = {
    number: 2262,
    title: '[sight] lookup fails',
    html_url: 'https://github.com/alibaba/anolisa/issues/2262',
    state,
    user: { login: author },
    labels: labels.map((name) => ({ name })),
    assignees: assignees.map((login) => ({ login })),
  };
  const github = {
    paginate: async () => comments,
    rest: {
      repos: {
        getContent: async () => ({
          data: {
            type: 'file',
            content: Buffer.from(JSON.stringify(metadata)).toString('base64'),
          },
        }),
      },
      issues: {
        get: async () => ({ data: issue }),
        getLabel: async () => ({ data: {} }),
        createLabel: async (args) => calls.push(['createLabel', args]),
        addLabels: async (args) => calls.push(['addLabels', args]),
        addAssignees: async (args) => {
          calls.push(['addAssignees', args]);
          const logins = acceptedAssignees ?? args.assignees;
          return {
            data: {
              ...issue,
              assignees: logins.map((login) => ({ login })),
            },
          };
        },
        createComment: async (args) => {
          calls.push(['createComment', args]);
          return { data: { id: 9001 } };
        },
        listComments: async () => ({ data: comments }),
      },
    },
  };
  const outputs = {};
  const core = {
    notice: () => {},
    warning: () => {},
    setOutput: (key, value) => {
      outputs[key] = String(value);
    },
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
    outputs,
    context: { repo: { owner: 'alibaba', repo: 'anolisa' } },
  };
}

test('parses a valid decision', () => {
  const decision = triage.parseDecision(inputEnv());
  assert.equal(decision.issueNumber, 2262);
  assert.equal(decision.component, 'sight');
  assert.equal(decision.confidence, 0.96);
  assert.equal(decision.source, 'classifier');
});

test('rejects an unknown decision source', () => {
  assert.throws(
    () => triage.parseDecision(inputEnv({ INPUT_DECISION_SOURCE: 'manual' })),
    /decision_source must be classifier or structured-form/
  );
});

test('rejects internal components', () => {
  assert.throws(
    () => triage.selectComponent(metadata, 'osbase'),
    /not available for public issue triage/
  );
});

test('uses default triagers when a public component list is empty', () => {
  assert.deepEqual(
    triage.selectTriagers(metadata, { issue_triagers: [] }),
    ['fallback']
  );
});

test('limits automatic assignment to configured authors', () => {
  const policy = triage.parsePolicy(
    JSON.stringify({ auto_assign_authors: ['trusted-reporter'] })
  );
  assert.equal(triage.shouldAutoAssign(policy, 'trusted-reporter'), true);
  assert.equal(triage.shouldAutoAssign(policy, 'external-user'), false);
});

test('defaults to no automatic assignment without an external policy', () => {
  assert.deepEqual(triage.parsePolicy(''), { autoAssignAuthors: [] });
});

test('rejects an invalid external assignment policy', () => {
  assert.throws(
    () => triage.parsePolicy('{invalid'),
    /ISSUE_TRIAGE_POLICY must be valid JSON/
  );
});

test('reports the repository path when component metadata cannot be read', async () => {
  const harness = makeHarness();
  harness.github.rest.repos.getContent = async () => {
    throw new Error('Not Found');
  };
  await assert.rejects(
    () => triage.run({ ...harness, env: inputEnv() }),
    /failed to read alibaba\/anolisa:\.github\/components\.json: Not Found/
  );
  assert.deepEqual(harness.calls, []);
});

test('dry run performs no GitHub mutations', async () => {
  const harness = makeHarness();
  await triage.run({ ...harness, env: inputEnv({ INPUT_APPLY: 'false' }) });
  assert.deepEqual(harness.calls, []);
  assert.equal(harness.outputs.mutated, 'false');
});

test('dry run and shadow mode do not retry a pending notification', async () => {
  for (const overrides of [
    { INPUT_APPLY: 'false' },
    { AUTOMATION_MODE: 'shadow' },
  ]) {
    const harness = makeHarness({
      comments: [
        {
          id: 42,
          user: { login: 'github-actions[bot]', type: 'Bot' },
          body: '<!-- anolisa-ai-triage:anolisa-2262-v1 -->',
        },
      ],
    });
    await triage.run({ ...harness, env: inputEnv(overrides) });
    assert.deepEqual(harness.calls, []);
    assert.equal(harness.outputs.notification_required, 'false');
    assert.equal(harness.outputs.triage_comment_id, '');
  }
});

test('rejects a closed issue before any mutation', async () => {
  const harness = makeHarness({ state: 'closed' });
  await assert.rejects(
    () => triage.run({ ...harness, env: inputEnv() }),
    /closed issue/
  );
  assert.deepEqual(harness.calls, []);
});

test('an existing manual component label takes precedence', async () => {
  const harness = makeHarness({ labels: ['component:anolisa'] });
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(harness.calls, []);
  assert.equal(harness.outputs.mutated, 'false');
});

test('applies a label and comment without assigning an external reporter', async () => {
  const harness = makeHarness();
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['addLabels', 'createComment']
  );
  assert.equal(harness.outputs.assigned, '');
  assert.equal(harness.outputs.mutated, 'true');
  assert.equal(harness.outputs.notification_required, 'true');
  assert.equal(harness.outputs.triage_comment_id, '9001');
});

test('assigns component owners for an allowlisted reporter', async () => {
  const harness = makeHarness({ author: 'trusted-reporter' });
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['addLabels', 'addAssignees', 'createComment']
  );
  assert.equal(harness.outputs.assigned, 'chengshuyi,jfeng18');
});

test('applies structured form routing only for an allowlisted reporter', async () => {
  const harness = makeHarness({ author: 'trusted-reporter' });
  await triage.run({
    ...harness,
    env: inputEnv({ INPUT_DECISION_SOURCE: 'structured-form' }),
  });
  const comment = harness.calls.find(([name]) => name === 'createComment')[1];
  assert.match(comment.body, /ANOLISA Issue Router/);
  assert.doesNotMatch(comment.body, /Routing source/);
  assert.doesNotMatch(comment.body, /assistance/);
});

test('rejects structured form routing for an external reporter', async () => {
  const harness = makeHarness();
  await assert.rejects(
    () => triage.run({
      ...harness,
      env: inputEnv({ INPUT_DECISION_SOURCE: 'structured-form' }),
    }),
    /requires an allowlisted reporter/
  );
  assert.deepEqual(harness.calls, []);
});

test('reports only owners accepted by GitHub as assignees', async () => {
  const harness = makeHarness({
    author: 'trusted-reporter',
    acceptedAssignees: ['chengshuyi'],
  });
  await triage.run({ ...harness, env: inputEnv() });
  assert.equal(harness.outputs.assigned, 'chengshuyi');
  const comment = harness.calls.find(([name]) => name === 'createComment')[1];
  assert.match(comment.body, /Assigned: @chengshuyi/);
  assert.doesNotMatch(comment.body, /Assigned:.*@jfeng18/);
});

test('does not apply a low-confidence decision', async () => {
  const harness = makeHarness();
  await triage.run({
    ...harness,
    env: inputEnv({ INPUT_CONFIDENCE: '0.60' }),
  });
  assert.deepEqual(harness.calls, []);
  assert.equal(harness.outputs.mutated, 'false');
});

test('ignores a forged decision marker from an untrusted commenter', async () => {
  const harness = makeHarness({
    comments: [
      {
        id: 41,
        user: { login: 'external-user', type: 'User' },
        body: '<!-- anolisa-ai-triage:anolisa-2262-v1 -->',
      },
    ],
  });
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(
    harness.calls.map(([name]) => name),
    ['addLabels', 'createComment']
  );
});

test('trusted decision marker retries an incomplete notification', async () => {
  const harness = makeHarness({
    assignees: ['chengshuyi'],
    comments: [
      {
        id: 42,
        user: { login: 'github-actions[bot]', type: 'Bot' },
        body: '<!-- anolisa-ai-triage:anolisa-2262-v1 -->',
      },
    ],
  });
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(harness.calls, []);
  assert.equal(harness.outputs.mutated, 'false');
  assert.equal(harness.outputs.notification_required, 'true');
  assert.equal(harness.outputs.triage_comment_id, '42');
  assert.equal(harness.outputs.assigned, 'chengshuyi');
});

test('notification marker suppresses a duplicate notification', async () => {
  const harness = makeHarness({
    comments: [
      {
        id: 43,
        user: { login: 'github-actions[bot]', type: 'Bot' },
        body: [
          '<!-- anolisa-ai-triage:anolisa-2262-v1 -->',
          '<!-- anolisa-ai-triage-notified:anolisa-2262-v1 -->',
        ].join('\n'),
      },
    ],
  });
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(harness.calls, []);
  assert.equal(harness.outputs.notification_required, 'false');
});

test('records notification state on the trusted triage comment', async () => {
  const calls = [];
  const body = '<!-- anolisa-ai-triage:anolisa-2262-v1 -->\nSummary';
  const github = {
    rest: {
      issues: {
        getComment: async () => ({
          data: {
            user: { login: 'github-actions[bot]' },
            body,
          },
        }),
        updateComment: async (args) => calls.push(args),
      },
    },
  };
  const updated = await triage.recordNotification({
    github,
    context: { repo: { owner: 'alibaba', repo: 'anolisa' } },
    commentId: 42,
    decisionId: 'anolisa-2262-v1',
  });
  assert.equal(updated, true);
  assert.equal(calls.length, 1);
  assert.match(calls[0].body, /anolisa-ai-triage-notified:anolisa-2262-v1/);
});

test('refuses to record notification state on an untrusted comment', async () => {
  const github = {
    rest: {
      issues: {
        getComment: async () => ({
          data: {
            user: { login: 'external-user' },
            body: '<!-- anolisa-ai-triage:anolisa-2262-v1 -->',
          },
        }),
      },
    },
  };
  await assert.rejects(
    () =>
      triage.recordNotification({
        github,
        context: { repo: { owner: 'alibaba', repo: 'anolisa' } },
        commentId: 42,
        decisionId: 'anolisa-2262-v1',
      }),
    /not a trusted decision record/
  );
});

test('escapes claim markers in classifier-authored public text', async () => {
  const harness = makeHarness();
  const marker = '<!-- anolisa-claim owner=attacker state=claimed -->';
  await triage.run({
    ...harness,
    env: inputEnv({ INPUT_SUMMARY: marker, INPUT_EVIDENCE: marker }),
  });
  const comment = harness.calls.find(([name]) => name === 'createComment')[1];
  assert.equal(comment.body.includes(marker), false);
  assert.equal(comment.body.includes('&lt;!-- anolisa-claim'), true);
});

test('rejects unknown automation modes without mutations', async () => {
  const harness = makeHarness();
  await assert.rejects(
    () => triage.run({ ...harness, env: inputEnv({ AUTOMATION_MODE: 'shdow' }) }),
    /COMMUNITY_AUTOMATION_MODE must be apply or shadow/
  );
  assert.deepEqual(harness.calls, []);
});

test('preserves every conflicting manual component label', async () => {
  const harness = makeHarness({
    labels: ['component:sight', 'component:anolisa'],
  });
  await triage.run({ ...harness, env: inputEnv() });
  assert.deepEqual(harness.calls, []);
  assert.equal(harness.outputs.mutated, 'false');
});
