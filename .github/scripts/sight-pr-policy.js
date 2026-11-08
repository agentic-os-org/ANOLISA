'use strict';

const { workLimit, quotaExempt } = require('./sight-work-limits');

function validationErrors(body) {
  const text = String(body || '').replace(/<!--[\s\S]*?-->/g, '')
    .replace(/^\s*(`{3,}|~{3,})[^\n]*\n[\s\S]*?^\s*\1\s*$/gm, '');
  const fields = text.split(/\r?\n/).map(line =>
    line.match(/^\s*(?:-\s*)?Regression evidence:\s*(.*?)\s*$/)).filter(Boolean);
  if (fields.length > 1) return ['Duplicate field: Regression evidence'];
  const value = (fields[0]?.[1] || '').replace(/^`(.*)`$/, '$1');
  if (!value || /^(?:<.*>|todo|tbd|n\/a|none)$/i.test(value)) {
    return ['Complete Regression evidence; name the regression case or explain why it is not applicable.'];
  }
  return [];
}

async function touchesSight(github, repository, pr) {
  const files = await github.paginate(github.rest.pulls.listFiles, {
    ...repository, pull_number: pr.number, per_page: 100,
  });
  if (files.length !== pr.changed_files) {
    throw new Error(`Incomplete changed-file list for #${pr.number}; retry the check.`);
  }
  return files.some((file) => [file.filename, file.previous_filename]
    .some((path) => path?.startsWith('src/agentsight/')));
}

function urgent(pr) {
  return (pr.labels || []).some((label) =>
    ['priority:p0', 'priority:p1'].includes(label.name));
}

const STATUS_CONTEXT = 'sight/submission-policy';

function fingerprint(pulls) {
  return JSON.stringify(pulls.map(pr => [pr.number, pr.head.sha, pr.body,
    pr.user.login.toLowerCase(), pr.draft, pr.state, urgent(pr), pr.changed_files, pr.base.ref]));
}

async function connectedSnapshot(github, repository, number, login, seedHeads, discovered = async () => {}) {
  const pulls = await github.paginate(github.rest.pulls.list, {
    ...repository, state: 'open', per_page: 100,
  });
  const authors = new Set([login.toLowerCase()]);
  const heads = new Set(seedHeads);
  const selected = new Map();
  // Statuses belong to a SHA, so include all authors sharing any affected head,
  // then their other PRs to calculate each author's quota independently.
  let expanded;
  do {
    expanded = false;
    for (const pr of pulls) {
      if (selected.has(pr.number) ||
          !authors.has(pr.user.login.toLowerCase()) && !heads.has(pr.head.sha)) continue;
      selected.set(pr.number, pr);
      authors.add(pr.user.login.toLowerCase());
      heads.add(pr.head.sha);
      expanded = true;
    }
  } while (expanded);
  await discovered([...selected.values()]);
  // A close/merge event also clears the triggering PR's obsolete admission.
  const numbers = new Set([...selected.keys(), number]);
  const details = [];
  for (const pull_number of [...numbers].sort((a, b) => a - b)) {
    const { data } = await github.rest.pulls.get({ ...repository, pull_number });
    details.push(data);
  }
  return details;
}

async function evaluatePulls(github, repository, pulls, env) {
  const applicable = [];
  const results = new Map();
  for (const pr of pulls) {
    const errors = [];
    results.set(pr.number, errors);
    if (pr.state !== 'open' || pr.draft ||
        !/^(main|release\/.+)$/.test(pr.base.ref)) continue;
    if (await touchesSight(github, repository, pr)) {
      applicable.push(pr);
      errors.push(...validationErrors(pr.body));
    }
  }
  const byAuthor = new Map();
  for (const pr of applicable.filter(pr => !urgent(pr))) {
    const login = pr.user.login.toLowerCase();
    if (!byAuthor.has(login)) byAuthor.set(login, []);
    byAuthor.get(login).push(pr);
  }
  for (const [login, ordinary] of byAuthor) {
    if (await quotaExempt(github, repository, login)) continue;
    const limit = workLimit(env.SIGHT_MAX_READY_PRS, 'SIGHT_MAX_READY_PRS');
    // Snapshot order is by PR number, so the oldest ready PRs can finish first.
    const admitted = ordinary.slice(0, limit).map(pr => `#${pr.number}`).join(', ');
    for (const pr of ordinary.slice(limit)) {
      results.get(pr.number).push(`The external contributor limit is ${limit} ordinary AgentSight PRs awaiting review: ${admitted}. Keep this PR Draft or finish earlier work.`);
    }
  }
  return results;
}

async function run({ github, context, core, env = process.env }) {
  const repository = context.repo;
  const { number, user: { login } } = context.payload.pull_request;
  const seedHeads = new Set([context.payload.pull_request.head.sha]);
  if (context.payload.before) seedHeads.add(context.payload.before);
  const knownHeads = new Set(seedHeads);
  const target_url = `${context.serverUrl || 'https://github.com'}/${repository.owner}/${repository.repo}/actions/runs/${context.runId}`;
  const publish = (sha, state, description) => github.rest.repos.createCommitStatus({
    ...repository, sha, state, context: STATUS_CONTEXT, target_url, description,
  });
  const invalidate = async (state, description) => {
    // Attempt every known head even if publishing one result fails.
    const attempts = await Promise.allSettled([...knownHeads].map(sha => publish(sha, state, description)));
    if (attempts.some(result => result.status === 'rejected')) {
      throw new Error('Cannot invalidate every AgentSight policy status; rerun this workflow.');
    }
  };
  try {
    // Queued runs always read live state, including when their event head is stale.
    // Repository-level queued execution also serializes authors sharing a SHA.
    for (let attempt = 0; attempt < 3; attempt++) {
      const pulls = await connectedSnapshot(github, repository, number, login, knownHeads, async candidates => {
        for (const pr of candidates) knownHeads.add(pr.head.sha);
        // Revoke previous results before fetching details or computing a new quota.
        await invalidate('pending', 'Refreshing affected AgentSight regression evidence and review slots.');
      });
      for (const pr of pulls) {
        if (!knownHeads.has(pr.head.sha)) {
          knownHeads.add(pr.head.sha);
          await publish(pr.head.sha, 'pending', 'Refreshing current-head AgentSight evidence and review slots.');
        }
      }
      const results = await evaluatePulls(github, repository, pulls, env);
      const unchanged = async () => fingerprint(pulls) === fingerprint(
        await connectedSnapshot(github, repository, number, login, knownHeads));
      if (!await unchanged()) continue;
      // A shared head cannot represent contradictory PR decisions: fail if any fails.
      const byHead = new Map();
      for (const pr of pulls) {
        const errors = byHead.get(pr.head.sha) || [];
        errors.push(...results.get(pr.number));
        byHead.set(pr.head.sha, errors);
      }
      // Revoke stale green results before granting newly available slots.
      for (const [sha, errors] of [...byHead].sort((a, b) => Number(!a[1].length) - Number(!b[1].length))) {
        await publish(sha, errors.length ? 'failure' : 'success', errors.length ?
          'AgentSight evidence or review-slot requirements are not met; see workflow summary.' :
          'AgentSight admission passed or does not apply; build/test results are separate required CI checks.');
      }
      if (!await unchanged()) {
        await invalidate('pending', 'Related PR state changed during publication; refreshing again.');
        continue;
      }
      await core.summary.addHeading('AgentSight submission evidence')
        .addRaw(pulls.map(pr => `#${pr.number} — head \`${pr.head.sha}\` — [CI checks](${context.serverUrl || 'https://github.com'}/${repository.owner}/${repository.repo}/pull/${pr.number}/checks): ` +
          (results.get(pr.number).join('\n') || 'Passed or not applicable.')).join('\n\n') +
          '\n\nHeads come from GitHub automatically. Build/test results come from the separate required CI checks (including Test agentsight); this admission status does not certify CI success or runtime coverage.')
        .write();
      // Required workflows attest trusted execution, not mutable admission.
      // A later edit or released slot can recover the status without a new commit.
      core.notice('Policy refresh completed; admission decisions are recorded in sight/submission-policy.');
      return;
    }
    throw new Error('Related PR state kept changing; retry the policy refresh.');
  } catch (error) {
    await invalidate('error', 'AgentSight policy refresh failed; rerun the workflow before merging.');
    throw error;
  }
}

module.exports = { run, validationErrors, touchesSight, STATUS_CONTEXT };
