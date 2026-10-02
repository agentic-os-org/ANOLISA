'use strict';

const DECISION_ID_RE = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const TRUSTED_BOT_LOGIN = 'github-actions[bot]';
const DECISION_SOURCES = new Set(['classifier', 'structured-form']);

function unique(values) {
  return [...new Set((values || []).filter(Boolean))];
}

function isTrustedBotComment(comment) {
  return comment.user?.login === TRUSTED_BOT_LOGIN;
}

function sanitizePublicText(value) {
  return String(value).replaceAll('<', '&lt;').replaceAll('>', '&gt;');
}

function hasTrustedFirstLineMarker(comment, marker) {
  if (!isTrustedBotComment(comment)) return false;
  const firstLine = String(comment.body || '').split(/\r?\n/, 1)[0].trim();
  return firstLine === marker;
}

function parseAutomationMode(value) {
  if (!['apply', 'shadow'].includes(value)) {
    throw new Error('COMMUNITY_AUTOMATION_MODE must be apply or shadow');
  }
  return value;
}

function parsePolicy(rawPolicy) {
  if (!rawPolicy) return { autoAssignAuthors: [] };

  let policy;
  try {
    policy = JSON.parse(rawPolicy);
  } catch (error) {
    throw new Error(`ISSUE_TRIAGE_POLICY must be valid JSON: ${error.message}`);
  }
  if (!policy || Array.isArray(policy) || typeof policy !== 'object') {
    throw new Error('ISSUE_TRIAGE_POLICY must be a JSON object');
  }

  const authors = policy.auto_assign_authors || [];
  if (
    !Array.isArray(authors) ||
    authors.some((author) => typeof author !== 'string' || !author.trim())
  ) {
    throw new Error(
      'ISSUE_TRIAGE_POLICY auto_assign_authors must be an array of non-empty strings'
    );
  }
  return {
    autoAssignAuthors: unique(
      authors.map((author) => author.trim().toLowerCase())
    ),
  };
}

function parseDecision(env) {
  const issueNumber = Number(env.INPUT_ISSUE_NUMBER);
  const confidence = Number(env.INPUT_CONFIDENCE);
  const component = String(env.INPUT_COMPONENT || '').trim().toLowerCase();
  const summary = String(env.INPUT_SUMMARY || '').trim();
  const evidence = String(env.INPUT_EVIDENCE || '').trim();
  const source = String(env.INPUT_DECISION_SOURCE || 'classifier').trim();
  const decisionId = String(env.INPUT_DECISION_ID || '').trim();

  if (!Number.isInteger(issueNumber) || issueNumber <= 0) {
    throw new Error('issue_number must be a positive integer');
  }
  if (!Number.isFinite(confidence) || confidence < 0 || confidence > 1) {
    throw new Error('confidence must be between 0 and 1');
  }
  if (!component) throw new Error('component must not be empty');
  if (!summary || summary.length > 2000) {
    throw new Error('summary must contain 1 to 2000 characters');
  }
  if (evidence.length > 2000) {
    throw new Error('evidence must contain at most 2000 characters');
  }
  if (!DECISION_SOURCES.has(source)) {
    throw new Error('decision_source must be classifier or structured-form');
  }
  if (!DECISION_ID_RE.test(decisionId)) {
    throw new Error('decision_id must be a safe identifier of at most 128 characters');
  }

  return {
    issueNumber,
    confidence,
    component,
    summary,
    evidence,
    source,
    decisionId,
    applyRequested: String(env.INPUT_APPLY).toLowerCase() === 'true',
  };
}

function selectComponent(metadata, componentId) {
  const component = (metadata.components || []).find(
    (candidate) => candidate.id === componentId
  );
  if (!component) throw new Error(`unknown component: ${componentId}`);
  if (component.status === 'internal') {
    throw new Error(`component is not available for public issue triage: ${componentId}`);
  }
  if (!component.label || !component.label.startsWith('component:')) {
    throw new Error(`component has an invalid label: ${componentId}`);
  }
  return component;
}

function shouldAutoAssign(policy, author) {
  return policy.autoAssignAuthors.includes(String(author).toLowerCase());
}

function assignedOwners(issue, owners) {
  const assignees = new Set(
    (issue.assignees || []).map((assignee) => assignee.login.toLowerCase())
  );
  return owners.filter((owner) => assignees.has(owner.toLowerCase()));
}

function selectTriagers(metadata, component) {
  const componentTriagers = component.issue_triagers || [];
  const configured = componentTriagers.length
    ? componentTriagers
    : metadata.defaults?.issue_triagers;
  return unique(configured);
}

async function readJsonFile(github, owner, repo, path) {
  let response;
  try {
    response = await github.rest.repos.getContent({ owner, repo, path });
  } catch (error) {
    throw new Error(
      `failed to read ${owner}/${repo}:${path}: ${error.message}`
    );
  }
  if (Array.isArray(response.data) || response.data.type !== 'file') {
    throw new Error(`${owner}/${repo}:${path} is not a regular file`);
  }
  try {
    return JSON.parse(
      Buffer.from(response.data.content, 'base64').toString('utf8')
    );
  } catch (error) {
    throw new Error(
      `${owner}/${repo}:${path} must contain valid JSON: ${error.message}`
    );
  }
}

async function ensureLabel(github, owner, repo, name, displayName) {
  try {
    await github.rest.issues.getLabel({ owner, repo, name });
  } catch (error) {
    if (error.status !== 404) throw error;
    await github.rest.issues.createLabel({
      owner,
      repo,
      name,
      color: '1d76db',
      description: `Issue or pull request for ${displayName}`,
    });
  }
}

function setIssueOutputs(core, issue, componentLabel) {
  core.setOutput('issue_title', issue.title || '');
  core.setOutput('issue_url', issue.html_url || '');
  core.setOutput('issue_author', issue.user?.login || '');
  core.setOutput('component_label', componentLabel);
}

async function writeSummary(core, rows) {
  await core.summary
    .addHeading('Issue router', 3)
    .addTable([
      [
        { data: 'Field', header: true },
        { data: 'Value', header: true },
      ],
      ...rows,
    ])
    .write();
}

async function recordNotification({
  github,
  context,
  commentId,
  decisionId,
}) {
  const numericCommentId = Number(commentId);
  if (!Number.isInteger(numericCommentId) || numericCommentId <= 0) {
    throw new Error('triage_comment_id must be a positive integer');
  }
  if (!DECISION_ID_RE.test(decisionId)) {
    throw new Error('decision_id must be a safe identifier of at most 128 characters');
  }
  const { owner, repo } = context.repo;
  const triageMarker = `<!-- anolisa-ai-triage:${decisionId} -->`;
  const notificationMarker =
    `<!-- anolisa-ai-triage-notified:${decisionId} -->`;
  const response = await github.rest.issues.getComment({
    owner,
    repo,
    comment_id: numericCommentId,
  });
  const comment = response.data;
  if (!hasTrustedFirstLineMarker(comment, triageMarker)) {
    throw new Error('triage comment is not a trusted decision record');
  }
  if ((comment.body || '').includes(notificationMarker)) return false;
  await github.rest.issues.updateComment({
    owner,
    repo,
    comment_id: numericCommentId,
    body: `${comment.body}\n${notificationMarker}`,
  });
  return true;
}

async function run({ github, context, core, env }) {
  const decision = parseDecision(env);
  const policy = parsePolicy(env.ISSUE_TRIAGE_POLICY);
  const repositoryMode = parseAutomationMode(env.AUTOMATION_MODE);
  const { owner, repo } = context.repo;
  const metadata = await readJsonFile(
    github,
    owner,
    repo,
    '.github/components.json'
  );
  const component = selectComponent(metadata, decision.component);
  const owners = selectTriagers(metadata, component);
  const minimumConfidence = Number(env.MIN_CONFIDENCE || '0.80');
  if (!Number.isFinite(minimumConfidence) || minimumConfidence < 0 || minimumConfidence > 1) {
    throw new Error('ISSUE_TRIAGE_MIN_CONFIDENCE must be between 0 and 1');
  }

  const response = await github.rest.issues.get({
    owner,
    repo,
    issue_number: decision.issueNumber,
  });
  const issue = response.data;
  if (issue.pull_request) throw new Error('issue_number refers to a pull request');
  if (issue.state !== 'open') throw new Error('issue_number refers to a closed issue');
  if (
    decision.source === 'structured-form' &&
    !shouldAutoAssign(policy, issue.user?.login || '')
  ) {
    throw new Error(
      'structured-form routing requires an allowlisted reporter'
    );
  }

  setIssueOutputs(core, issue, component.label);
  core.setOutput('owners', owners.join(','));
  core.setOutput('assigned', assignedOwners(issue, owners).join(','));
  core.setOutput('mutated', 'false');
  core.setOutput('notification_required', 'false');
  core.setOutput('triage_comment_id', '');

  const currentLabels = (issue.labels || []).map((label) =>
    typeof label === 'string' ? label : label.name
  );
  const currentComponents = currentLabels.filter((label) =>
    label.startsWith('component:')
  );
  const conflictingComponents = currentComponents.filter(
    (label) => label !== component.label
  );
  if (conflictingComponents.length) {
    core.notice(
      `Manual component labels ${conflictingComponents.join(', ')} ` +
        `take precedence over ${component.label}.`
    );
    await writeSummary(core, [
      ['Status', 'manual override'],
      ['Issue', `#${decision.issueNumber}`],
      ['Existing components', conflictingComponents.join(', ')],
      ['Routing component', component.label],
      ['Source', decision.source],
    ]);
    return;
  }

  const apply = decision.applyRequested && repositoryMode === 'apply';
  if (!apply || decision.confidence < minimumConfidence) {
    const status = apply ? 'low confidence' : 'dry run';
    await writeSummary(core, [
      ['Status', status],
      ['Issue', `#${decision.issueNumber}`],
      ['Component', component.label],
      ['Source', decision.source],
      ['Confidence', decision.confidence.toFixed(2)],
      ['Minimum confidence', minimumConfidence.toFixed(2)],
    ]);
    return;
  }

  const marker = `<!-- anolisa-ai-triage:${decision.decisionId} -->`;
  const notificationMarker =
    `<!-- anolisa-ai-triage-notified:${decision.decisionId} -->`;
  const comments = await github.paginate(github.rest.issues.listComments, {
    owner,
    repo,
    issue_number: decision.issueNumber,
    per_page: 100,
  });
  const triageComment = comments.find(
    (comment) => hasTrustedFirstLineMarker(comment, marker)
  );
  if (triageComment) {
    const notificationRequired = !(triageComment.body || '').includes(
      notificationMarker
    );
    core.setOutput('notification_required', String(notificationRequired));
    core.setOutput('triage_comment_id', String(triageComment.id));
    core.notice(`Decision ${decision.decisionId} was already applied.`);
    await writeSummary(core, [
      ['Status', 'already applied'],
      ['Issue', `#${decision.issueNumber}`],
      ['Component', component.label],
    ]);
    return;
  }

  await ensureLabel(github, owner, repo, component.label, component.display_name);
  if (!currentComponents.length) {
    await github.rest.issues.addLabels({
      owner,
      repo,
      issue_number: decision.issueNumber,
      labels: [component.label],
    });
  }

  let assigned = assignedOwners(issue, owners);
  if (shouldAutoAssign(policy, issue.user?.login || '') && owners.length) {
    try {
      const assignmentResponse = await github.rest.issues.addAssignees({
        owner,
        repo,
        issue_number: decision.issueNumber,
        assignees: owners,
      });
      assigned = assignedOwners(assignmentResponse.data, owners);
    } catch (error) {
      core.warning(`Unable to assign component owners: ${error.message}`);
    }
  }

  const ownerMentions = owners.length
    ? owners.map((login) => `@${login}`).join(' ')
    : 'No component owner is currently configured.';
  const assignmentText = assigned.length
    ? `Assigned: ${assigned.map((login) => `@${login}`).join(' ')}`
    : 'No assignee was added; assignment records active implementation ownership.';
  const comment = [
    marker,
    '## 🔀 Issue Router',
    '',
    `- Component: \`${component.id}\``,
    `- Confidence: \`${Math.round(decision.confidence * 100)}%\``,
    `- Owners notified: ${ownerMentions}`,
    `- ${assignmentText}`,
    '',
    sanitizePublicText(decision.summary),
    ...(decision.evidence
      ? ['', `Evidence: ${sanitizePublicText(decision.evidence)}`]
      : []),
    '',
    '_This routing was applied by the ANOLISA Issue Router through GitHub Actions._',
  ].join('\n');

  const commentResponse = await github.rest.issues.createComment({
    owner,
    repo,
    issue_number: decision.issueNumber,
    body: comment,
  });

  core.setOutput('assigned', assigned.join(','));
  core.setOutput('mutated', 'true');
  core.setOutput('notification_required', 'true');
  core.setOutput('triage_comment_id', String(commentResponse.data.id));
  await writeSummary(core, [
    ['Status', 'applied'],
    ['Issue', `#${decision.issueNumber}`],
    ['Component', component.label],
    ['Source', decision.source],
    ['Confidence', decision.confidence.toFixed(2)],
    ['Owners', owners.join(', ') || 'none'],
    ['Assigned', assigned.join(', ') || 'none'],
  ]);
}

module.exports = {
  parseDecision,
  parsePolicy,
  recordNotification,
  sanitizePublicText,
  run,
  selectComponent,
  selectTriagers,
  shouldAutoAssign,
};
