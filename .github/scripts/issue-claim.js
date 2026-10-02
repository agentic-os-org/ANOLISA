'use strict';

const TRUSTED_BOT_LOGIN = 'github-actions[bot]';
const CLAIM_MARKER_RE =
  /^<!-- anolisa-claim owner=([A-Za-z0-9][A-Za-z0-9-]{0,38}) state=(claimed|released) -->$/;

const LABEL_SPECS = {
  'status:accepted': {
    color: '0e8a16',
    description: 'Accepted and ready to be worked on',
  },
  'status:in-progress': {
    color: '1d76db',
    description: 'Someone is actively working on this',
  },
};

const CLAIMABLE_LABELS = new Set([
  'status:accepted',
  'status:ready',
  'good first issue',
  'help wanted',
  'action:helpwanted',
]);

const READY_STATUS_LABELS = [
  'status:accepted',
  'status:ready',
];

function parseAutomationMode(value) {
  if (!['apply', 'shadow'].includes(value)) {
    throw new Error('COMMUNITY_AUTOMATION_MODE must be apply or shadow');
  }
  return value;
}

function parseCommand(body) {
  const firstLine = String(body || '').split(/\r?\n/, 1)[0].trim();
  const match = firstLine.match(/^\/(claim|unclaim|assign|unassign)\s*$/);
  if (!match) return null;
  return ['claim', 'assign'].includes(match[1]) ? 'claim' : 'unclaim';
}

function parseClaimMarker(comment) {
  if (comment.user?.login !== TRUSTED_BOT_LOGIN) return null;
  const firstLine = String(comment.body || '').split(/\r?\n/, 1)[0].trim();
  const match = firstLine.match(CLAIM_MARKER_RE);
  return match ? { owner: match[1], state: match[2] } : null;
}

function labelNames(issue) {
  return new Set(
    (issue.labels || []).map((label) =>
      typeof label === 'string' ? label : label.name
    )
  );
}

function workflowStatusLabels(labels) {
  return [...labels].filter((label) => label.startsWith('status:'));
}

async function run({ github, context, core, env }) {
  const { owner, repo } = context.repo;
  const issueNumber = context.payload.issue.number;
  const actor = context.payload.comment.user.login;
  const association = context.payload.comment.author_association || 'NONE';
  const mode = parseAutomationMode(env.AUTOMATION_MODE);
  const command = parseCommand(context.payload.comment.body);
  if (!command) {
    core.notice('Unsupported claim command.');
    return;
  }
  const isMaintainer = ['OWNER', 'MEMBER', 'COLLABORATOR'].includes(
    association
  );

  async function ensureLabel(name) {
    const spec = LABEL_SPECS[name];
    try {
      await github.rest.issues.getLabel({ owner, repo, name });
    } catch (error) {
      if (error.status !== 404) throw error;
      await github.rest.issues.createLabel({
        owner,
        repo,
        name,
        color: spec.color,
        description: spec.description,
      });
    }
  }

  async function removeLabelIfPresent(name, labels) {
    if (!labels.has(name)) return;
    try {
      await github.rest.issues.removeLabel({
        owner,
        repo,
        issue_number: issueNumber,
        name,
      });
    } catch (error) {
      if (error.status !== 404) throw error;
    }
  }

  async function post(body) {
    if (mode === 'shadow') {
      core.notice(`[shadow] ${body.replace(/\n/g, ' ')}`);
      return;
    }
    await github.rest.issues.createComment({
      owner,
      repo,
      issue_number: issueNumber,
      body,
    });
  }

  const { data: issue } = await github.rest.issues.get({
    owner,
    repo,
    issue_number: issueNumber,
  });
  if (issue.state !== 'open') {
    core.notice(`Issue #${issueNumber} is closed; claim commands are ignored.`);
    return;
  }
  const labels = labelNames(issue);
  const comments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: issueNumber,
    per_page: 100,
  });
  let latestClaim = null;
  for (const comment of comments) {
    const parsed = parseClaimMarker(comment);
    if (parsed) latestClaim = parsed;
  }
  const activeClaim = latestClaim?.state === 'claimed' ? latestClaim.owner : null;

  await core.summary
    .addHeading('Issue claim command', 3)
    .addTable([
      [
        { data: 'Field', header: true },
        { data: 'Value', header: true },
      ],
      ['Mode', mode],
      ['Command', command],
      ['Actor', actor],
      ['Active claim', activeClaim || 'none'],
    ])
    .write();

  if (command === 'claim') {
    const recovering = activeClaim === actor;
    const eligible = [...CLAIMABLE_LABELS].some((label) => labels.has(label));
    const allowedStatuses = new Set([
      ...READY_STATUS_LABELS,
      ...(recovering ? ['status:in-progress'] : []),
    ]);
    const incompatibleStatus = workflowStatusLabels(labels).find(
      (label) => !allowedStatuses.has(label)
    );
    if (activeClaim && !recovering) {
      await post(
        `@${actor}, this issue is already claimed by @${activeClaim}. ` +
          'Please coordinate in the thread or wait for `/unclaim`.'
      );
      return;
    }
    if (incompatibleStatus) {
      await post(
        `@${actor}, this issue cannot be claimed while it has ` +
          `\`${incompatibleStatus}\`. A maintainer must update its status first.`
      );
      return;
    }
    if (!recovering && !eligible) {
      await post(
        `@${actor}, this issue is not ready to claim yet. ` +
          'A maintainer must first mark it accepted or add `good first issue` or `help wanted`.'
      );
      return;
    }
    if (mode === 'shadow') {
      core.notice(`[shadow] would claim #${issueNumber} for @${actor}`);
      return;
    }

    if (!recovering) {
      await post(
        [
          `<!-- anolisa-claim owner=${actor} state=claimed -->`,
          `@${actor} claimed this issue.`,
          '',
          'Please post a short progress update if the implementation direction changes. ' +
            'Use `/unclaim` to release it.',
          '',
          '> Formal assignment is best-effort; this claim marker is authoritative.',
        ].join('\n')
      );
    }

    await ensureLabel('status:in-progress');
    for (const status of READY_STATUS_LABELS) {
      await removeLabelIfPresent(status, labels);
    }
    await github.rest.issues.addLabels({
      owner,
      repo,
      issue_number: issueNumber,
      labels: ['status:in-progress'],
    });
    try {
      await github.rest.issues.addAssignees({
        owner,
        repo,
        issue_number: issueNumber,
        assignees: [actor],
      });
    } catch (error) {
      core.warning(`Formal assignment failed: ${error.message}`);
    }
    return;
  }

  const recoveringRelease =
    latestClaim?.state === 'released' &&
    (actor === latestClaim.owner || isMaintainer);
  if (!activeClaim && !recoveringRelease) {
    core.notice('No active claim to release.');
    return;
  }
  const claimOwner = activeClaim || latestClaim.owner;
  if (actor !== claimOwner && !isMaintainer) {
    await post(
      `@${actor}, only @${claimOwner} or a maintainer can release this claim.`
    );
    return;
  }
  if (mode === 'shadow') {
    core.notice(`[shadow] would release #${issueNumber} from @${claimOwner}`);
    return;
  }

  const preservedStatuses = workflowStatusLabels(labels).filter(
    (label) => label !== 'status:in-progress'
  );
  const nonReadyStatuses = preservedStatuses.filter(
    (label) => !READY_STATUS_LABELS.includes(label)
  );
  const availability = nonReadyStatuses.length
    ? `The issue remains ${nonReadyStatuses.map((label) => `\`${label}\``).join(', ')} ` +
      'and is not available for another contributor until a maintainer updates it.'
    : 'The issue is available for another contributor.';
  if (activeClaim) {
    await post(
      [
        `<!-- anolisa-claim owner=${claimOwner} state=released -->`,
        `@${actor} released the claim previously held by @${claimOwner}.`,
        '',
        availability,
      ].join('\n')
    );
  }

  const refreshed = await github.rest.issues.get({
    owner,
    repo,
    issue_number: issueNumber,
  });
  const refreshedLabels = labelNames(refreshed.data);
  await removeLabelIfPresent('status:in-progress', refreshedLabels);
  const remainingStatuses = workflowStatusLabels(refreshedLabels).filter(
    (label) => label !== 'status:in-progress'
  );
  if (!remainingStatuses.length) {
    await ensureLabel('status:accepted');
    await github.rest.issues.addLabels({
      owner,
      repo,
      issue_number: issueNumber,
      labels: ['status:accepted'],
    });
  }
  try {
    await github.rest.issues.removeAssignees({
      owner,
      repo,
      issue_number: issueNumber,
      assignees: [claimOwner],
    });
  } catch (error) {
    core.warning(`Unable to remove formal assignee: ${error.message}`);
  }
}

module.exports = {
  parseClaimMarker,
  parseCommand,
  run,
};
