'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');
const { readPublication, issueRevision, isPublishable, publicationMarker } = require('./issue-analysis');
const metadata = require('../components.json');

function notificationData({ snapshot, analysis, review, expectedPriority }, context, publication) {
  if (!isPublishable(analysis) && analysis.scope.status !== 'scope_unresolved' &&
      analysis.disposition !== 'scope_unresolved') return null;
  const refuted = analysis.claims.length > 0 && analysis.claims.every((claim) => claim.assessment === 'refuted');
  const needsHuman = review.verdict !== 'accept' || analysis.scope.status === 'scope_unresolved' ||
    analysis.disposition === 'scope_unresolved' || (!expectedPriority && !refuted);
  const component = metadata.components.find((item) => item.id === snapshot.component.id);
  if (!component) throw new Error('Unknown notification component');
  const triagers = component.issue_triagers?.length ? component.issue_triagers : metadata.defaults.issue_triagers;
  const base = `https://github.com/${snapshot.repository.full_name}`;
  const nextStep = needsHuman ? review.unresolved[0] || analysis.unresolved[0] || 'Maintainer: resolve the missing evidence or review objection.' :
    ['priority:p0', 'priority:p1'].includes(expectedPriority) ? 'Maintainer: review the priority and execute the verification plan.' :
      analysis.claims[0]?.verification_plan || 'Maintainer: confirm the scope and schedule the backlog item.';
  return {
    issue_number: snapshot.issue.number, issue_title: snapshot.issue.title,
    issue_url: `${base}/issues/${snapshot.issue.number}`, component_label: component.label,
    priority: expectedPriority || (refuted ? 'refuted' : 'unresolved'),
    conclusion: analysis.summary.conclusion, impact: analysis.summary.impact,
    next_step: nextStep, owners: triagers.join(', '),
    report_url: publication.comment_url || `${base}/actions/runs/${context.runId}`,
    needs_human: needsHuman,
  };
}

function sendDingTalk(data, env) {
  const result = spawnSync('python3', [path.join(__dirname, 'dingtalk_notification.py'), 'send-analysis'], {
    input: JSON.stringify(data), encoding: 'utf8', timeout: 40_000, maxBuffer: 16 * 1024,
    env: { PATH: process.env.PATH, DINGTALK_WEBHOOK: env.DINGTALK_WEBHOOK, DINGTALK_SECRET: env.DINGTALK_SECRET || '' },
  });
  try {
    const status = JSON.parse(result.stdout).status;
    if (status === 'delivered' && result.status === 0 && !result.error) return 'delivered';
    if (status === 'rejected') return 'rejected';
  } catch {}
  return 'unknown';
}

async function notify({ github, context, core, env, send = sendDingTalk }) {
  if (!env.DINGTALK_WEBHOOK) {
    core.notice('DingTalk result notification is disabled: webhook is not configured');
    return { status: 'disabled' };
  }
  const decision = await readPublication({ github, context, env });
  const { snapshot, expectedPriority, analysis } = decision;
  const publication = JSON.parse(fs.readFileSync(path.join(env.ANALYSIS_ROOT, 'output/writeback-results.json')));
  if (!['verified', 'skipped'].includes(publication.status)) throw new Error('Publication must finish before notification');
  const data = notificationData(decision, context, publication);
  if (!data) return { status: 'skipped' };
  const { data: current } = await github.rest.issues.get({ ...context.repo, issue_number: snapshot.issue.number });
  const expected = expectedPriority ? { ...snapshot.issue, labels: [...snapshot.issue.labels, { name: expectedPriority }] } : snapshot.issue;
  if (issueRevision(current) !== issueRevision(expected)) throw new Error('Issue changed before notification');
  if (isPublishable(analysis) && !publication.comment_url) throw new Error('Missing verified publication link');
  const key = crypto.createHash('sha256').update(JSON.stringify({
    issue_id: snapshot.issue.id, component: snapshot.component.id, priority: data.priority,
    conclusion: data.conclusion, impact: data.impact, next_step: data.next_step, needs_human: data.needs_human,
  })).digest('hex');
  const marker = `<!-- issue-analysis-notification:${snapshot.issue.id}:${key} -->`;
  const params = { ...context.repo, issue_number: snapshot.issue.number };
  const comments = await github.paginate(github.rest.issues.listComments, { ...params, per_page: 100 });
  let receipt = comments.find((comment) => comment.user?.login === 'github-actions[bot]' && comment.body?.split('\n')[0] === marker);
  if (receipt) {
    const state = JSON.parse(receipt.body.split('\n')[1]);
    if (state.status === 'delivered') return { status: 'already_delivered' };
    if (state.status !== 'rejected') throw new Error('DingTalk delivery outcome is uncertain; inspect the receipt before retrying');
  }
  if (isPublishable(analysis)) {
    const report = comments.find((comment) => comment.user?.login === 'github-actions[bot]' &&
      comment.html_url === publication.comment_url);
    if (!report) throw new Error('Published analysis report was deleted; rerun analysis before notification');
    const { data: verified } = await github.rest.issues.getComment({ ...context.repo, comment_id: report.id });
    const expectedBody = `${publicationMarker(snapshot, analysis, decision.review)}\n` +
      fs.readFileSync(path.join(env.ANALYSIS_ROOT, 'output/comment.md'), 'utf8');
    if (verified?.user?.login !== 'github-actions[bot]' || verified.body !== expectedBody ||
        verified.html_url !== publication.comment_url) {
      throw new Error('Published analysis report changed; rerun analysis before notification');
    }
  }
  const record = async (status) => {
    const body = `${marker}\n${JSON.stringify({ status, run_id: context.runId, input_fingerprint: snapshot.input_fingerprint })}`;
    const { data: written } = receipt ? await github.rest.issues.updateComment({ ...context.repo, comment_id: receipt.id, body }) :
      await github.rest.issues.createComment({ ...params, body });
    receipt = written;
    const { data: verified } = await github.rest.issues.getComment({ ...context.repo, comment_id: written.id });
    if (verified.body !== body) throw new Error('Notification receipt readback failed');
  };
  await record('sending');
  let status = 'unknown';
  try { status = await send(data, env); } catch {}
  if (!['delivered', 'rejected'].includes(status)) status = 'unknown';
  await record(status);
  if (status !== 'delivered') throw new Error(`DingTalk result notification ${status}; inspect its receipt`);
  core.info(`Notified triagers for Issue #${snapshot.issue.number}`);
  return { status, receipt_id: receipt.id };
}

module.exports = { notify, notificationData, sendDingTalk };
