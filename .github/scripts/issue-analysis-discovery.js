'use strict';

const { assertRepository, authorizeActor, priorityRules, inputFingerprint, hasCompletedAnalysis } = require('./issue-analysis');
const { enabledComponents, selectIssue, componentScope } = require('./issue-analysis-config');
const metadata = require('../components.json');

async function discover({ github, context, core, env }) {
  const { data: repository } = await github.rest.repos.get(context.repo);
  assertRepository(repository);
  await authorizeActor(github, context);
  const enabled = enabledComponents(env.ISSUE_ANALYSIS_COMPONENTS);
  const mode = env.INPUT_MODE || 'single';
  if (!['single', 'batch'].includes(mode)) throw new Error('mode must be single or batch');
  const limit = Number(env.INPUT_MAX_ISSUES || 10);
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 25) {
    throw new Error('max_issues must be between 1 and 25');
  }
  const selected = [];
  const skipped = {};
  const truncated = [];
  let source = '';
  let rules;
  const resolveSource = async () => {
    if (source) return;
    const { data: commit } = await github.rest.repos.getCommit({ ...context.repo, ref: env.INPUT_TARGET_REF || 'main' });
    if (!/^[a-f0-9]{40}$/.test(commit.sha)) throw new Error('GitHub did not resolve a full commit SHA');
    source = commit.sha;
  };
  const consider = async (issue) => {
    const decision = selectIssue(issue, enabled);
    let reason = decision.reason;
    if (!reason && mode === 'batch') {
      await resolveSource();
      if (!rules) rules = priorityRules(await github.paginate(github.rest.issues.listLabelsForRepo, { ...context.repo, per_page: 100 }));
      const snapshot = { repository, issue, component: decision.component, scope: componentScope(decision.component),
        target: { source_commit_sha: source }, rules };
      snapshot.input_fingerprint = inputFingerprint(snapshot);
      const comments = await github.paginate(github.rest.issues.listComments, { ...context.repo, issue_number: issue.number, per_page: 100 });
      if (hasCompletedAnalysis(snapshot, comments)) reason = 'already_analyzed';
    }
    if (reason) skipped[reason] = (skipped[reason] || 0) + 1;
    else if (!selected.includes(issue.number)) selected.push(issue.number);
  };
  if (!enabled.length) skipped.analysis_disabled = 1;
  else if (mode === 'single') {
    const number = Number(env.INPUT_ISSUE_NUMBER);
    if (!Number.isSafeInteger(number) || number <= 0) throw new Error('issue_number must be a positive integer');
    const { data: issue } = await github.rest.issues.get({ ...context.repo, issue_number: number });
    await consider(issue);
  } else {
    for (const id of enabled) {
      const component = metadata.components.find((item) => item.id === id);
      for (let page = 1; page <= 10 && selected.length < limit; page++) {
        const { data: issues } = await github.rest.issues.listForRepo({
          ...context.repo, state: 'open', labels: component.label, sort: 'created', direction: 'asc', per_page: 100, page,
        });
        for (const issue of issues) {
          if (selected.length >= limit) break;
          await consider(issue);
        }
        if (issues.length < 100) break;
        if (page === 10) truncated.push(id);
      }
      if (selected.length >= limit) break;
    }
  }
  if (selected.length) await resolveSource();
  core.setOutput('issues', JSON.stringify(selected));
  core.setOutput('has_issues', String(selected.length > 0));
  core.setOutput('target_sha', source);
  await core.summary.addHeading('Issue analysis selection', 3).addCodeBlock(JSON.stringify({
    mode, enabled_components: enabled, selected, skipped, source_commit_sha: source,
    scan_limit_components: truncated, max_issues: limit,
  }, null, 2), 'json').write();
  return { selected, skipped, truncated };
}

module.exports = { discover };
