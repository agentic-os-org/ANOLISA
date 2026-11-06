'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { execFileSync, spawnSync } = require('node:child_process');

const CLI_VERSION = '1.1.77';
const MODEL_ALIAS = 'auto';
const { componentForIssue, componentScope, enabledComponents, selectIssue } = require('./issue-analysis-config');
const PRIORITIES = ['priority:p0', 'priority:p1', 'priority:p2', 'priority:p3'];
const PUBLICATION_BUDGET = 1600;
const VERIFICATION_NOTICE = 'Source analysis only; no dynamic reproduction or functional tests were executed for this Issue.';
const sha256 = (value) => crypto.createHash('sha256').update(value).digest('hex');
const readJson = (file) => JSON.parse(fs.readFileSync(file, 'utf8'));

function agentTimeoutMs() {
  const seconds = Number(process.env.AGENT_TIMEOUT_SECONDS || 0);
  check(Number.isSafeInteger(seconds) && seconds >= 0, 'AGENT_TIMEOUT_SECONDS must be a nonnegative integer');
  return seconds * 1000;
}

function check(condition, message) {
  if (!condition) throw new Error(message);
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function containsCredential(raw, token) {
  const text = raw.toString();
  // Inspect JSON escapes even when the surrounding document cannot be parsed.
  const decoded = text.replace(/\\(?:u[0-9a-fA-F]{4}|["\\/bfnrt])/g,
    (escape) => JSON.parse(`"${escape}"`));
  return text.includes(token) || decoded.includes(token);
}

function readResultFile(directory, phase) {
  const file = path.join(directory, `${phase}.json`);
  const stat = fs.lstatSync(file);
  check(stat.isFile() && !stat.isSymbolicLink() && stat.size <= 256 * 1024 &&
    fs.realpathSync(file) === file, 'Agent must produce one regular JSON result file of at most 256 KiB');
  return fs.readFileSync(file);
}

function assertRepository(repository) {
  check(repository.full_name.toLowerCase() === 'agentic-os-org/anolisa' && repository.id === 1195873793,
    'Issue analysis only accepts the verified agentic-os-org/ANOLISA repository');
}

async function authorizeActor(github, context) {
  check(context.actor !== 'github-actions[bot]', 'Only a repository writer can start Issue analysis');
  const { data: actor } = await github.rest.repos.getCollaboratorPermissionLevel({
    ...context.repo, username: context.actor,
  });
  check(['admin', 'maintain', 'write'].includes(actor.permission), 'Only a repository writer can start Issue analysis');
  return actor.permission;
}

function priorityRules(labels) {
  return PRIORITIES.map((label) => {
    const definition = labels.find((item) => item.name === label)?.description;
    const rule = { label, definition: typeof definition === 'string' ? definition.trim() : '' };
    return { rule_id: `priority:${sha256(JSON.stringify(rule))}`, ...rule };
  });
}

function inputFingerprint(snapshot) {
  return sha256(JSON.stringify({
    repository_id: snapshot.repository.id, issue_id: snapshot.issue.id,
    title: snapshot.issue.title, body: snapshot.issue.body, state: snapshot.issue.state,
    labels: snapshot.issue.labels.map((label) => typeof label === 'string' ? label : label.name)
      .filter((name) => !name.startsWith('priority:')).sort(),
    component: snapshot.component, scope: snapshot.scope,
    source_commit_sha: snapshot.target.source_commit_sha,
    rules: snapshot.rules.map((rule) => rule.rule_id),
  }));
}


async function prepare({ github, context, core, env }) {
  const { data: repository } = await github.rest.repos.get(context.repo);
  assertRepository(repository);
  const number = Number(env.INPUT_ISSUE_NUMBER);
  check(Number.isSafeInteger(number) && number > 0, 'issue_number must be a positive integer');
  const params = { ...context.repo, issue_number: number };
  const { data: issue } = await github.rest.issues.get(params);
  check(!issue.pull_request, 'Pull requests are not issue-analysis inputs');
  const actorPermission = await authorizeActor(github, context);
  const root = env.ANALYSIS_ROOT;
  const run = {
    run_id: `${context.runId}-${env.GITHUB_RUN_ATTEMPT || '1'}`,
    workflow_commit_sha: context.sha, actor: context.actor, actor_permission: actorPermission,
    repository: { full_name: repository.full_name, id: repository.id },
    issue_id: issue.id, issue_number: number, started_at_utc: new Date().toISOString(),
    permissions: { github_write: false, modify_source: false,
      publish_issue_results: env.PUBLISH_ALLOWED === 'true' },
  };
  const selection = selectIssue(issue, enabledComponents(env.ISSUE_ANALYSIS_COMPONENTS));
  const priorities = issue.labels.map((label) => typeof label === 'string' ? label : label.name)
    .filter((name) => name.startsWith('priority:'));
  core.setOutput('issue_number', String(number));
  const skipReason = selection.reason || '';
  core.setOutput('skip_analysis', String(Boolean(skipReason)));
  if (skipReason) {
    const reason = priorities.length ? `Issue #${number} already has priority labels: ${priorities.join(', ')}` : skipReason;
    writeJson(path.join(root, 'output/run.json'), { ...run, stage: skipReason,
      reason, existing_priority_labels: priorities, finished_at_utc: new Date().toISOString() });
    writeManifest(root);
    core.notice(reason);
    return;
  }
  const component = selection.component;
  core.setOutput('component', component.id);
  const requestedRef = env.INPUT_TARGET_REF || repository.default_branch;
  const { data: commit } = await github.rest.repos.getCommit({ ...context.repo, ref: requestedRef });
  check(/^[a-f0-9]{40}$/.test(commit.sha), 'GitHub did not resolve a full commit SHA');
  // Keep only publication metadata; discussion is not an analysis input.
  const comments = (await github.paginate(github.rest.issues.listComments, { ...params, per_page: 100 }))
    .filter((comment) => comment.user?.login === 'github-actions[bot]' &&
      (comment.body?.startsWith(commentPrefix({ issue, component })) ||
        comment.body?.startsWith(completionPrefix({ issue, component }))));
  const labels = await github.paginate(github.rest.issues.listLabelsForRepo, { ...context.repo, per_page: 100 });
  const { data: latest } = await github.rest.issues.get(params);
  check(!latest.pull_request && issueRevision(latest) === issueRevision(issue),
    'Issue changed during collection; rerun to obtain a fresh snapshot');
  const capturedAt = new Date().toISOString();
  const snapshot = {
    template_version: '1.2', mode: 'single_issue_source_analysis',
    repository: { full_name: repository.full_name, id: repository.id },
    issue: { id: issue.id, number: issue.number, title: issue.title, body: issue.body,
      state: issue.state, labels: issue.labels },
    comments, labels,
    target: { requested_ref: requestedRef, source_commit_sha: commit.sha },
    component,
    scope: componentScope(component),
    rules: priorityRules(labels),
    captured_at_utc: capturedAt,
    discovery: {
      candidate_count: 1, issue_ids: [issue.id], selection: `explicit issue #${number}`,
      labels_complete: true,
      consistency: 'Paginated reads are not an atomic GitHub snapshot.',
      full_query_enumeration: false,
      linked_pr_verification: 'Merge ancestry and linked fixes are not verified.',
    },
  };
  snapshot.input_fingerprint = inputFingerprint(snapshot);
  writeJson(path.join(root, 'output/snapshot.json'), snapshot);
  if (env.SKIP_COMPLETED === 'true' && hasCompletedAnalysis(snapshot, comments)) {
    core.setOutput('skip_analysis', 'true');
    writeJson(path.join(root, 'output/run.json'), { ...run, stage: 'already_analyzed',
      target: snapshot.target, finished_at_utc: new Date().toISOString() });
    writeManifest(root);
    core.notice('An analysis of these exact inputs has already completed');
    return;
  }
  const agentSnapshot = { ...snapshot };
  delete agentSnapshot.comments;
  delete agentSnapshot.labels;
  writeJson(path.join(root, 'input/snapshot.json'), agentSnapshot);
  writeJson(path.join(root, 'input/agent-snapshot.json'), agentSnapshot);
  writeJson(path.join(root, 'output/agent-snapshot.json'), agentSnapshot);
  writeJson(path.join(root, 'output/run.json'), {
    ...run, target: snapshot.target,
    snapshot_sha256: sha256(JSON.stringify(snapshot)), cli_version: CLI_VERSION,
    review: 'same_agent_review', stage: 'snapshot_complete',
    limits: { agent_timeout_seconds: agentTimeoutMs() / 1000 || null,
      agent_timeout_policy: agentTimeoutMs() ? 'configured_phase_budget' : 'workflow_job_deadline_only',
      memory_mib: 4096, cpus: 2, pids: 256, tmpfs_mib: 256 },
  });
  core.setOutput('target_sha', commit.sha);
}

function stageSource(checkout, root) {
  const snapshot = readJson(path.join(root, 'input/snapshot.json'));
  const actualSha = execFileSync('git', ['-C', checkout, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  check(actualSha === snapshot.target.source_commit_sha, 'Checkout does not match the frozen SHA');
  check(execFileSync('git', ['-C', checkout, 'status', '--porcelain'], { encoding: 'utf8' }).trim() === '',
    'Target checkout must be clean');
  const files = execFileSync('git', ['-C', checkout, 'ls-files', '-z'], { maxBuffer: 8 * 1024 * 1024 })
    .toString('utf8').split('\0').filter(Boolean);
  const manifest = { source_commit_sha: actualSha, files: Object.create(null), omitted: [] };
  let bytes = 0;
  for (const name of files) {
    const source = path.resolve(checkout, name);
    check(source.startsWith(`${path.resolve(checkout)}${path.sep}`), 'Source path escaped checkout');
    const stat = fs.lstatSync(source);
    if (!stat.isFile() || fs.realpathSync(source) !== source || stat.size > 1024 * 1024 ||
        /(^|\/)(\.qoder|\.claude|\.codex|\.agents|\.mcp\.json)(\/|$)/.test(name)) {
      manifest.omitted.push({ path: name, reason: 'Not a regular small source file, or agent configuration' });
      continue;
    }
    const content = fs.readFileSync(source);
    if (content.includes(0)) {
      manifest.omitted.push({ path: name, reason: 'Binary file' });
      continue;
    }
    bytes += content.length;
    check(bytes <= 128 * 1024 * 1024, 'Source snapshot exceeds the 128 MiB budget');
    const destination = path.join(root, 'source', name);
    fs.mkdirSync(path.dirname(destination), { recursive: true });
    fs.writeFileSync(destination, content);
    manifest.files[name] = { sha256: sha256(content), lines: content.toString('utf8').split('\n').length };
  }
  for (const entry of snapshot.scope.entrypoints) {
    check(manifest.files[entry], `Missing component entrypoint: ${entry}`);
  }
  writeJson(path.join(root, 'input/source-manifest.json'), manifest);
  writeJson(path.join(root, 'output/source-manifest.json'), manifest);
}

function object(value, label) {
  check(value && typeof value === 'object' && !Array.isArray(value), `${label} must be an object`);
}

function text(value, label) {
  check(typeof value === 'string' && value.trim().length > 0 && value.length <= 8000,
    `${label} must contain 1 to 8000 characters`);
}

function list(value, label, maximum = 50) {
  check(Array.isArray(value) && value.length <= maximum, `${label} must be an array of at most ${maximum} items`);
}

function textList(value, label) {
  list(value, label);
  value.forEach((item) => text(item, label));
}

function choice(value, choices, label) {
  check(choices.includes(value), `Invalid ${label}: ${JSON.stringify(value)}`);
}

function parseResult(raw) {
  check(Buffer.byteLength(raw) <= 256 * 1024, 'Agent result exceeds 256 KiB');
  const trimmed = raw.trim();
  const fenced = /^```(?:json)?\s*\n([\s\S]*)\n```$/i.exec(trimmed);
  const value = JSON.parse(fenced ? fenced[1] : trimmed);
  object(value, 'Agent result');
  check(value.schema_version === 1, 'Unsupported result schema');
  return value;
}

function parseCliResult(raw) {
  const packet = JSON.parse(raw.trim());
  object(packet, 'CLI result');
  check(packet.type === 'result' && packet.subtype === 'success' && packet.is_error === false,
    'Qoder did not return a successful final result');
  check(typeof packet.result === 'string', 'Qoder final result must contain text');
  return packet;
}

function isPublishable(analysis) {
  return analysis.disposition === 'active' &&
    ['in_scope', 'partially_in_scope', 'v1_only'].includes(analysis.scope.status);
}

function validateAnalysis(value, snapshot, sourceRoot, manifest) {
  check(value.issue_number === snapshot.issue.number, 'Result refers to a different issue');
  check(value.source_commit_sha === snapshot.target.source_commit_sha, 'Result refers to a different commit');
  object(value.summary, 'summary');
  for (const field of ['conclusion', 'impact', 'priority_reason']) {
    text(value.summary[field], `summary.${field}`);
    check(value.summary[field].length <= 1000 && !/\b[CE]\d{2}\b/.test(value.summary[field]),
      `summary.${field} must omit internal IDs and stay within the 1000-character input limit`);
  }
  object(value.scope, 'scope');
  choice(value.scope.status, ['in_scope', 'partially_in_scope', 'v1_only', 'out_of_scope', 'scope_unresolved'], 'scope');
  text(value.scope.reason, 'scope.reason');
  choice(value.disposition, ['active', 'fixed', 'withdrawn', 'duplicate', 'out_of_scope', 'scope_unresolved'], 'disposition');
  text(value.disposition_reason, 'disposition_reason');
  choice(value.problem_type, ['bug', 'enhancement', 'test_gap', 'documentation_gap', 'contract_unresolved', 'mixed', 'not_applicable'], 'problem_type');
  list(value.evidence, 'evidence');
  const evidenceIds = new Set();
  for (const evidence of value.evidence) {
    object(evidence, 'evidence');
    check(/^E\d{2}$/.test(evidence.id) && !evidenceIds.has(evidence.id), 'Invalid or duplicate evidence ID');
    evidenceIds.add(evidence.id);
    check(evidence.kind === 'static', 'Source analysis cannot claim dynamic evidence');
    const entry = manifest.files[evidence.path];
    check(entry && Object.hasOwn(manifest.files, evidence.path), 'Evidence path is outside the source manifest');
    check(Number.isInteger(evidence.line_start) && Number.isInteger(evidence.line_end) &&
      evidence.line_start > 0 && evidence.line_end >= evidence.line_start &&
      evidence.line_end <= entry.lines && evidence.line_end - evidence.line_start < 200,
    'Invalid evidence line range');
    const content = fs.readFileSync(path.join(sourceRoot, evidence.path), 'utf8');
    check(sha256(content) === entry.sha256, 'Evidence source changed after freezing');
    text(evidence.quote, 'evidence.quote');
    const excerpt = content.split('\n').slice(evidence.line_start - 1, evidence.line_end).join('\n');
    check(excerpt.includes(evidence.quote),
      `Evidence ${evidence.id} quote does not match ${evidence.path}:${evidence.line_start}-${evidence.line_end}`);
    text(evidence.supports, 'evidence.supports');
    text(evidence.does_not_establish, 'evidence.does_not_establish');
  }
  list(value.key_evidence_ids, 'key_evidence_ids');
  check((value.evidence.length === 0 || value.key_evidence_ids.length > 0) &&
    new Set(value.key_evidence_ids).size === value.key_evidence_ids.length &&
    value.key_evidence_ids.every((id) => evidenceIds.has(id)), 'Select unique existing key evidence IDs');
  list(value.claims, 'claims', 20);
  const claimIds = new Set();
  for (const claim of value.claims) {
    object(claim, 'claim');
    check(/^C\d{2}$/.test(claim.id) && !claimIds.has(claim.id), 'Invalid or duplicate claim ID');
    claimIds.add(claim.id);
    for (const field of ['statement', 'entrypoint', 'preconditions', 'contract', 'impact', 'limitations', 'verification_plan']) {
      text(claim[field], `claim.${field}`);
    }
    choice(claim.assessment, ['supported_by_source', 'partially_supported', 'insufficient_evidence', 'refuted'], 'claim assessment');
    textList(claim.evidence_ids, 'claim.evidence_ids');
    check(claim.evidence_ids.every((id) => evidenceIds.has(id)), 'Claim references missing evidence');
    if (claim.assessment !== 'insufficient_evidence') check(claim.evidence_ids.length > 0, 'Claim has no evidence');
  }
  if (value.claims.length > 0 && value.claims.every((claim) => claim.assessment === 'refuted')) {
    check(value.problem_type === 'not_applicable', 'Fully refuted reports must use problem_type=not_applicable');
  }
  object(value.impact, 'impact');
  text(value.impact.supported, 'impact.supported');
  text(value.impact.potential, 'impact.potential');
  object(value.confidence, 'confidence');
  for (const field of ['behavior', 'reachability', 'impact', 'priority']) text(value.confidence[field], `confidence.${field}`);
  textList(value.unresolved, 'unresolved');
  object(value.priority, 'priority');
  choice(value.priority.label, [null, ...PRIORITIES], 'priority label');
  textList(value.priority.rule_ids, 'priority.rule_ids');
  const ruleIds = new Set(snapshot.rules.map((rule) => rule.rule_id));
  check(value.priority.rule_ids.every((id) => ruleIds.has(id)), 'Unknown priority rule reference');
  for (const field of ['rationale', 'not_higher', 'not_lower', 'upgrade_conditions', 'downgrade_conditions']) {
    text(value.priority[field], `priority.${field}`);
  }
  const v1Only = value.scope.status === 'v1_only';
  if (v1Only) {
    check(snapshot.component.id === 'sec-core', 'V1-only backlog policy is restricted to SecCore');
    check(value.problem_type === 'not_applicable' && value.claims.length === 0,
      'V1-only triage must classify scope without claiming defect verification');
    check(value.evidence.length > 0, 'V1-only triage requires source evidence for its scope');
    if (value.disposition === 'active') check(value.priority.label === 'priority:p3',
      'Active V1-only issues must receive priority:p3 backlog triage');
  }
  if (value.priority.label !== null) {
    check(isPublishable(value),
      'Only active in-scope or V1-only issues can receive a priority suggestion');
    if (!v1Only) {
      check(snapshot.rules.every((rule) => rule.definition),
        'Missing GitHub priority definitions require an unresolved priority');
      const rank = PRIORITIES.indexOf(value.priority.label);
      const adjacent = PRIORITIES.slice(Math.max(0, rank - 1), rank + 2);
      check(snapshot.rules.filter((rule) => adjacent.includes(rule.label))
        .every((rule) => value.priority.rule_ids.includes(rule.rule_id)),
      'Priority must cite its own and adjacent definitions');
      check(value.claims.some((claim) => ['supported_by_source', 'partially_supported'].includes(claim.assessment)),
        'Priority suggestion has no supported claim');
    }
  }
  if (value.scope.status === 'out_of_scope') {
    check(value.disposition === 'out_of_scope' && value.problem_type === 'not_applicable',
      'Out-of-scope issues must not receive a problem type or another disposition');
  }
  if (['in_scope', 'partially_in_scope'].includes(value.scope.status)) check(value.claims.length > 0, 'In-scope issue has no claims');
  validateReview(inlineReview(value), value);
  return value;
}

function inlineReview(analysis) {
  object(analysis.self_review, 'self_review');
  return { schema_version: 1, issue_number: analysis.issue_number,
    source_commit_sha: analysis.source_commit_sha, independence: 'same_agent_review',
    verdict: analysis.self_review.verdict, summary: analysis.self_review.summary,
    challenges: analysis.self_review.challenges, unresolved: analysis.self_review.unresolved };
}

function reviewReasons(analysis) {
  const reasons = [];
  if (['priority:p0', 'priority:p1'].includes(analysis.priority.label)) reasons.push('highest_priority');
  const gradingText = [analysis.priority.rationale, analysis.priority.not_higher, analysis.priority.not_lower,
    analysis.priority.upgrade_conditions, analysis.priority.downgrade_conditions, analysis.confidence.priority].join('\n');
  if (analysis.priority.label !== null && /AGENTS\.md|\bPR[ -]review|pull[ -]request\s+review/i.test(gradingText)) {
    reasons.push('priority_authority_ambiguous');
  }
  if (analysis.self_review.verdict === 'needs_revision') reasons.push('self_review_objection');
  return reasons;
}

function reviewIfNeeded(root, policyFile) {
  const analysis = readJson(path.join(root, 'input/analysis.json'));
  const reasons = reviewReasons(analysis);
  if (reasons.length > 0) runAgent(root, 'review', policyFile);
  const file = path.join(root, 'output/run.json');
  writeJson(file, { ...readJson(file), review_execution: {
    strategy: reasons.length ? 'focused_model_review' : 'inline_self_review', reasons,
    interpretation: 'Both modes use the same agent; neither is independent review.' } });
  return reasons;
}

function validateReview(value, analysis) {
  check(value.independence === 'same_agent_review', 'Only same-agent review is supported');
  choice(value.verdict, ['accept', 'needs_revision'], 'review verdict');
  text(value.summary, 'review.summary');
  check(value.summary.length <= 1000, 'Review summary exceeds 1000 characters');
  list(value.challenges, 'review challenges', 20);
  check(value.challenges.length > 0, 'Review must challenge the analysis');
  for (const challenge of value.challenges) text(challenge, 'review challenge');
  textList(value.unresolved, 'review unresolved');
  check(value.source_commit_sha === analysis.source_commit_sha && value.issue_number === analysis.issue_number,
    'Review refers to a different analysis input');
  return value;
}

function phasePolicy(document, phase) {
  const analysisStart = document.indexOf('## Analysis phase');
  const reviewStart = document.indexOf('## Review phase');
  check(analysisStart > 0 && reviewStart > analysisStart, 'Agent policy is missing phase boundaries');
  return document.slice(0, analysisStart) + (phase === 'analysis' ?
    document.slice(analysisStart, reviewStart) : document.slice(reviewStart));
}

function agentArguments(root, phase, containerName, policy, resultDirectory, attempt = 1) {
  return [
    'run', '--name', containerName, '--read-only', '--cap-drop=ALL',
    '--security-opt=no-new-privileges', '--memory=4g', '--memory-swap=4g', '--cpus=2', '--pids-limit=256',
    '--tmpfs', '/tmp:rw,noexec,nosuid,size=128m,uid=1000,gid=1000,mode=700',
    '--tmpfs', '/home/node:rw,noexec,nosuid,size=128m,uid=1000,gid=1000,mode=700',
    '--mount', `type=bind,src=${path.resolve(root, 'source')},dst=/source,readonly`,
    '--mount', `type=bind,src=${path.resolve(root, 'input')},dst=/input,readonly`,
    '--mount', `type=bind,src=${path.resolve(resultDirectory)},dst=/result`,
    '--workdir=/source', '--env', 'QODER_PERSONAL_ACCESS_TOKEN',
    'issue-analysis:local', 'qodercli',
    '--config-dir', '/tmp/qoder', '--setting-sources', '',
    '--add-dir', '/input', '--add-dir', '/result',
    '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}',
    '--tools', 'Read', 'Grep', 'Glob', 'Write',
    '--allowed-tools', 'Read,Grep,Glob,Write(/result/**)',
    '--permission-mode', 'accept_edits', '--no-session-persistence', '--max-model-request-retries', '2',
    '--model', MODEL_ALIAS, '--output-format', 'stream-json', '--system-prompt', policy,
    '-p', `Execute only the ${phase} phase. Read /input/${attempt > 1 ? `${phase}-rejected` : phase === 'review' ? 'review-context' : 'agent-snapshot'}.json. ` +
      (fs.existsSync(path.join(root, 'input/review-before-revision.json')) ?
        (phase === 'analysis' ? 'Revise /input/analysis-before-revision.json using every material finding in /input/review-before-revision.json. Correct the claims, priority reasons, and public summary against the cited source; do not merely repeat the old report. ' :
          'Check the revised /input/analysis.json and whether each material finding in /input/review-before-revision.json has been resolved. ') : '') +
      (attempt > 1 ? `Repair the rejected /input/${phase}-rejected.json using its validation error and the complete required JSON schema. The validator stops at the first error: check all required fields and references before resubmitting. Preserve supported facts and citations; consult snapshot/source only when a correction needs them. ` : '') +
      'Query only relevant entries of /input/source-manifest.json with Grep; do not read the entire catalog. ' +
      `Source files are under /source. Use Write to save the required JSON to /result/${phase}.json. ` +
      'Only this result directory is writable; do not print the JSON as your final response.',
  ];
}

function runAgent(root, phase, policyFile, attempt = 1, phaseStarted = Date.now()) {
  choice(phase, ['analysis', 'review'], 'phase');
  if (phase === 'review') reviewContext(root);
  const token = process.env.QODER_PERSONAL_ACCESS_TOKEN;
  check(token, 'Configure QODER_PERSONAL_ACCESS_TOKEN in the repository Actions secrets');
  const run = readJson(path.join(root, 'output/run.json'));
  assertRepository(run.repository);
  const containerName = `issue-analysis-${crypto.randomUUID()}`;
  const startedAt = new Date().toISOString();
  const policy = phasePolicy(fs.readFileSync(policyFile, 'utf8'), phase);
  const resultDirectory = fs.mkdtempSync(path.join(root, `agent-${phase}-`));
  // The unprivileged container user differs from the hosted runner user.
  fs.chmodSync(resultDirectory, 0o777);
  let result;
  let cleanup;
  const phaseBudget = agentTimeoutMs();
  const timeout = phaseBudget ? phaseBudget - (Date.now() - phaseStarted) : 0;
  check(!phaseBudget || timeout > 0, 'Agent phase time budget exhausted');
  const args = agentArguments(root, phase, containerName, policy, resultDirectory, attempt);
  const trace = path.join(root, `output/${phase}-events-attempt-${attempt}.jsonl`);
  try {
    result = spawnSync(process.execPath, [path.join(__dirname, 'sec-core-issue-runtime.js'), trace, ...args], {
      encoding: 'utf8', timeout: timeout || undefined, maxBuffer: 2 * 1024 * 1024,
      stdio: ['ignore', 'pipe', 'pipe', 2],
      env: { PATH: process.env.PATH, QODER_PERSONAL_ACCESS_TOKEN: token },
    });
  } finally {
    // Killing the Docker client on timeout does not stop its container.
    cleanup = spawnSync('docker', ['rm', '--force', containerName], { stdio: 'ignore', timeout: 15_000 });
  }
  const execution = {
    phase, attempt, timeout_seconds: timeout ? Math.floor(timeout / 1000) : null,
    timeout_policy: phaseBudget ? 'configured_phase_budget' : 'workflow_job_deadline_only',
    event_trace: path.basename(trace),
    execution_kind: phase === 'review' ? 'focused_model_review' : 'model_analysis',
    started_at_utc: startedAt, finished_at_utc: new Date().toISOString(),
    exit_code: result.status, signal: result.signal, error_code: result.error?.code || null,
    stderr_tail: String(result.stderr || '').replaceAll(token, '[REDACTED]').slice(-4000),
    runtime: `qodercli ${CLI_VERSION}`, requested_model_alias: MODEL_ALIAS,
    actual_model: 'not_reported_by_cli', policy_sha256: sha256(policy),
    cleanup_result: cleanup.status === 0 ? 'removed' : 'removal_failed_or_container_not_created',
    validation: 'pending',
  };
  const executionFile = path.join(root, `output/${phase}-execution.json`);
  writeJson(executionFile, execution);
  let candidate;
  try {
    check(!result.error && result.status === 0, `Agent ${phase} failed (${result.error?.code || result.status}); no result accepted`);
    check(cleanup.status === 0, 'Agent container cleanup failed; no result accepted');
    check(!result.stdout.includes(token), 'Agent output contained its credential; output discarded');
    const packet = parseCliResult(result.stdout);
    check(!JSON.stringify(packet).includes(token), 'Agent output contained its credential; output discarded');
    const reported = Object.keys(packet.modelUsage || {});
    execution.actual_model = reported.join(', ') || 'not_reported_by_cli';
    execution.num_turns = packet.num_turns ?? null;
    execution.permission_denials = (packet.permission_denials || []).map((item) =>
      ['Read', 'Grep', 'Glob', 'Write'].includes(item.tool_name) ? item.tool_name : 'other');
    check(fs.readdirSync(resultDirectory).length === 1,
    'Agent must produce one regular JSON result file of at most 256 KiB');
    const raw = readResultFile(resultDirectory, phase).toString('utf8');
    check(!containsCredential(raw, token), 'Agent output contained its credential; output discarded');
    const value = parseResult(raw);
    check(!JSON.stringify(value).includes(token), 'Agent output contained its credential; output discarded');
    candidate = value;
    if (phase === 'analysis') {
      validateAnalysis(value, readJson(path.join(root, 'input/snapshot.json')),
        path.join(root, 'source'), readJson(path.join(root, 'input/source-manifest.json')));
    } else {
      validateReview(value, readJson(path.join(root, 'input/analysis.json')));
    }
    writeJson(path.join(root, `input/${phase}.json`), value);
    writeJson(path.join(root, `output/${phase}.json`), value);
    writeJson(executionFile, { ...execution, validation: 'passed' });
    if (phase === 'analysis') {
      const review = inlineReview(value);
      writeJson(path.join(root, 'input/review.json'), review);
      writeJson(path.join(root, 'output/review.json'), review);
      writeJson(path.join(root, 'output/review-execution.json'), {
        phase: 'review', execution_kind: 'inline_self_review', source_execution: 'analysis-execution.json',
        started_at_utc: execution.finished_at_utc, finished_at_utc: execution.finished_at_utc,
        num_turns: 0, validation: 'passed',
        interpretation: 'Self-check included in the analysis invocation; no second model call.' });
    }
  } catch (error) {
    const failed = { ...execution, validation: 'failed', failure: error.message };
    const name = `${phase}-raw-attempt-${attempt}.json`;
    const file = path.join(root, 'output', name);
    failed.raw_result = { accepted: false, retained: false };
    try {
      fs.rmSync(file, { force: true });
      check(cleanup.status === 0, 'Cannot retain a result while container cleanup is unconfirmed');
      const raw = readResultFile(resultDirectory, phase);
      failed.raw_result.original_sha256 = sha256(raw);
      failed.raw_result.size_bytes = raw.length;
      if (containsCredential(raw, token)) {
        failed.raw_result.reason = 'credential_detected';
      } else {
        fs.writeFileSync(file, raw);
        failed.raw_result.retained = true;
        failed.raw_result.path = name;
      }
    } catch (captureError) {
      failed.raw_result.reason = captureError.code || 'unreadable_or_unsafe_result';
    }
    writeJson(executionFile, failed);
    if (candidate) {
      const rejected = { accepted: false, validation_error: error.message, candidate };
      writeJson(path.join(root, `output/${phase}-rejected-attempt-${attempt}.json`), rejected);
      if (attempt < 2 && (!phaseBudget || phaseBudget - (Date.now() - phaseStarted) > 30_000)) {
        writeJson(path.join(root, `output/${phase}-execution-attempt-${attempt}.json`), failed);
        writeJson(path.join(root, `input/${phase}-rejected.json`), rejected);
        return runAgent(root, phase, policyFile, attempt + 1, phaseStarted);
      }
    }
    throw error;
  } finally {
    if (!candidate && cleanup.status === 0) {
      try {
        const raw = readResultFile(resultDirectory, phase).toString('utf8');
        const value = parseResult(raw);
        if (!containsCredential(raw, token) && !JSON.stringify(value).includes(token)) {
          writeJson(path.join(root, `output/${phase}-incomplete-attempt-${attempt}.json`), {
            accepted: false, reason: 'Runtime did not complete; preserved for debugging only.', candidate: value });
        }
      } catch { /* Raw failures are retained separately; unsafe files are never read. */ }
    }
    fs.rmSync(resultDirectory, { recursive: true, force: true });
  }
}

function reviewContext(root) {
  const snapshot = readJson(path.join(root, 'input/agent-snapshot.json'));
  const analysis = readJson(path.join(root, 'input/analysis.json'));
  const evidence = analysis.key_evidence_ids.slice(0, 3).map((id) => analysis.evidence.find((item) => item.id === id)).map((item) => {
    const lines = fs.readFileSync(path.join(root, 'source', item.path), 'utf8').split('\n');
    const start = Math.max(1, item.line_start - 12);
    return { ...item, context_line_start: start,
      context: lines.slice(start - 1, item.line_end + 12).map((line, i) => `${start + i}: ${line}`).join('\n') };
  });
  const value = { issue: { number: snapshot.issue.number, title: snapshot.issue.title, body: snapshot.issue.body },
    scope: snapshot.scope, rules: snapshot.rules,
    analysis: { ...analysis, evidence },
    verification_scope: VERIFICATION_NOTICE,
    controller_checks: 'All source hashes, exact quotes, line ranges, rule IDs, and key evidence references already validated.' };
  writeJson(path.join(root, 'input/review-context.json'), value);
  writeJson(path.join(root, 'output/review-context.json'), value);
  return value;
}

function revise(root, policyFile) {
  if (readJson(path.join(root, 'input/review.json')).verdict !== 'needs_revision') return false;
  check(!fs.existsSync(path.join(root, 'input/review-before-revision.json')), 'Only one review-driven revision is allowed');
  for (const phase of ['analysis', 'review']) {
    for (const directory of ['input', 'output']) {
      fs.copyFileSync(path.join(root, directory, `${phase}.json`), path.join(root, directory, `${phase}-before-revision.json`));
    }
    for (const name of fs.readdirSync(path.join(root, 'output')).filter((name) =>
      name.startsWith(`${phase}-`) && !name.includes('-before-revision'))) {
      fs.copyFileSync(path.join(root, 'output', name), path.join(root, 'output', name.replace(/(\.[^.]+)$/, '-before-revision$1')));
    }
    fs.rmSync(path.join(root, `input/${phase}-rejected.json`), { force: true });
  }
  const started = Date.now();
  runAgent(root, 'analysis', policyFile, 1, started);
  runAgent(root, 'review', policyFile, 1, started);
  return true;
}

function commentPrefix(snapshot) {
  return snapshot.component.id === 'sec-core' ? `<!-- sec-core-analysis:${snapshot.issue.id}:` :
    `<!-- issue-analysis:${snapshot.component.id}:${snapshot.issue.id}:`;
}

function completionPrefix(snapshot) {
  return `<!-- issue-analysis-complete:${snapshot.component.id}:${snapshot.issue.id}:`;
}

function hasCompletedAnalysis(snapshot, comments) {
  const completed = `${completionPrefix(snapshot)}${snapshot.input_fingerprint} -->`;
  const report = `${commentPrefix(snapshot)}${snapshot.input_fingerprint}:`;
  return comments.some((comment) => {
    if (comment.user?.login !== 'github-actions[bot]') return false;
    const firstLine = String(comment.body || '').split('\n')[0];
    return firstLine === completed || (firstLine.startsWith(report) &&
      /^[a-f0-9]{64} -->$/.test(firstLine.slice(report.length)));
  });
}

async function recordCompletion({ github, context, snapshot, analysis }) {
  const params = { ...context.repo, issue_number: snapshot.issue.number };
  const { data: current } = await github.rest.issues.get(params);
  check(!current.pull_request && issueRevision(current) === issueRevision(snapshot.issue),
    'Issue changed before recording analysis completion');
  const comments = await github.paginate(github.rest.issues.listComments, { ...params, per_page: 100 });
  const marker = `${completionPrefix(snapshot)}${snapshot.input_fingerprint} -->`;
  const previous = comments.find((comment) => comment.user?.login === 'github-actions[bot]' &&
    comment.body?.startsWith(completionPrefix(snapshot)));
  const body = `${marker}\nSource analysis completed: ${analysis.scope.status}; ${analysis.disposition}.\n` +
    `[Report](https://github.com/${snapshot.repository.full_name}/actions/runs/${context.runId}).`;
  if (previous && previous.body !== body) {
    const captured = snapshot.comments.find((comment) => comment.id === previous.id);
    check(captured?.body === previous.body, 'Analysis completion receipt changed; rerun before publication');
  }
  const { data: written } = previous?.body === body ? { data: previous } : previous ?
    await github.rest.issues.updateComment({ ...context.repo, comment_id: previous.id, body }) :
    await github.rest.issues.createComment({ ...params, body });
  const { data: verified } = await github.rest.issues.getComment({ ...context.repo, comment_id: written.id });
  const { data: latest } = await github.rest.issues.get(params);
  check(verified.body === body && !latest.pull_request && issueRevision(latest) === issueRevision(snapshot.issue),
    'Analysis completion readback did not match the snapshot');
  return { id: written.id, body_sha256: sha256(body) };
}

function publicationMarker(snapshot, analysis, review) {
  const digest = sha256(JSON.stringify({ analysis, review }));
  return `${commentPrefix(snapshot)}${snapshot.input_fingerprint}:${digest} -->`;
}

function commentOperation(snapshot, comment) {
  const previous = snapshot.comments.findLast((item) => item.user?.login === 'github-actions[bot]' &&
    item.body?.startsWith(commentPrefix(snapshot)));
  return previous ? { action: 'update_comment', comment_id: previous.id,
    previous_body_sha256: sha256(previous.body), comment_sha256: sha256(comment) } :
    { action: 'comment', comment_sha256: sha256(comment) };
}

function finish(root) {
  const snapshot = readJson(path.join(root, 'output/snapshot.json'));
  assertRepository(snapshot.repository);
  const repository = snapshot.repository.full_name;
  const analysis = readJson(path.join(root, 'input/analysis.json'));
  const review = readJson(path.join(root, 'input/review.json'));
  const priority = analysis.priority.label;
  const approvedPriority = review.verdict === 'accept' ? priority : null;
  const authorized = readJson(path.join(root, 'output/run.json')).permissions.publish_issue_results === true;
  const existing = snapshot.issue.labels.map((label) => typeof label === 'string' ? label : label.name);
  const plan = {
    authorized, repository, component: snapshot.component.id, issue_number: snapshot.issue.number,
    source_commit_sha: analysis.source_commit_sha, operations: [],
    suggested_priority: priority, approved_priority: approvedPriority,
    existing_priority_labels: existing.filter((label) => label.startsWith('priority:')),
    reason: authorized ? 'A separate trusted job may add an absent priority and publish a short summary.' :
      'Report-only run; no remote writes are authorized.',
    human_review_recommended: ['priority:p0', 'priority:p1'].includes(priority) || review.verdict !== 'accept',
  };
  // Escape agent text before rendering it in GitHub's Markdown UI.
  const escape = (value, maximum = Infinity) => {
    const parts = Array.from(String(value).replace(/\s+/g, ' ').trim(), (c) =>
      ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '@': '&#64;' }[c] ||
        (/[\\`*_{}\[\]()#+!|]/.test(c) ? `\\${c}` : c)));
    const full = parts.join('');
    if (full.length <= maximum) return full;
    let shortened = '';
    for (const part of parts) {
      if (shortened.length + part.length > maximum - 3) break;
      shortened += part;
    }
    return maximum >= 3 ? `${shortened.trimEnd()}...` : '';
  };
  const run = readJson(path.join(root, 'output/run.json'));
  const refuted = analysis.claims.length > 0 && analysis.claims.every((claim) => claim.assessment === 'refuted');
  const grade = priority || (refuted ? 'not applicable (reported claim refuted)' : 'unresolved');
  const verificationNotice = analysis.scope.status === 'v1_only' ?
    `V1-only backlog triage; the reported problem was not verified. ${VERIFICATION_NOTICE}` : VERIFICATION_NOTICE;
  const reviewText = review.summary.replace(/\b[CE]\d{2}\b/g, '').replace(/\s+/g, ' ').trim();
  const reviewExcerpt = reviewText.length > 180 ? `${reviewText.slice(0, 177)}...` : reviewText;
  const keyEvidence = analysis.key_evidence_ids.slice(0, 3).map((id) => analysis.evidence.find((item) => item.id === id));
  let nextStep;
  if (review.verdict !== 'accept') nextStep = `Resolve the review objection before changing priority: ${reviewExcerpt}`;
  else if (!priority && !refuted && analysis.disposition === 'active') nextStep = 'Maintainer: resolve the missing evidence or priority-rule ambiguity before assigning a grade.';
  const report = [
    `# ${snapshot.component.display_name} Issue #${snapshot.issue.number}`, '',
    `**Suggested priority: ${grade}${review.verdict === 'accept' ? '' : ' (provisional; review pending)'}**`, '',
    escape(analysis.summary.priority_reason), '',
    ...(review.verdict === 'accept' ? [] : [`Label update withheld. Review objection: ${escape(reviewExcerpt)}`, '']),
    '## Impact', '',
    `Source-supported: ${escape(analysis.impact.supported)}`, '',
    `Not established: ${escape(analysis.impact.potential)}`, '',
    '## Verification scope', '',
    verificationNotice, '',
    '## Key evidence', '',
    ...keyEvidence.map((item) =>
      `- [${escape(item.path.split('/').at(-1))}:${item.line_start}](https://github.com/${repository}/blob/${analysis.source_commit_sha}/${encodeURI(item.path)}#L${item.line_start}-L${item.line_end}) — ${escape(item.supports)}`), '',
    ...(nextStep ? ['## Next step', '', escape(nextStep), ''] : []),
    `Issue: https://github.com/${repository}/issues/${snapshot.issue.number}`,
    `Source: https://github.com/${repository}/tree/${analysis.source_commit_sha}`,
    `Scope: ${analysis.scope.status}; snapshot: ${snapshot.captured_at_utc}`, '',
  ].join('\n');
  fs.writeFileSync(path.join(root, 'output/report.md'), report);
  const renderComment = (summaryLimit) => [
    `### ${snapshot.component.display_name} analysis`, '',
    `**Priority: ${grade}${review.verdict === 'accept' ? '' : ' (provisional; review pending)'}**`,
    escape(analysis.summary.priority_reason, summaryLimit), '',
    `**Impact:** ${escape(analysis.summary.impact, summaryLimit)}`, '',
    `**Verification:** ${verificationNotice}`, '',
    ...(review.verdict === 'accept' ? [] : [`**Next step:** ${escape(reviewText, 180)} No priority label will be changed.`, '']),
    '<details><summary>Source and report</summary>', '',
    `Source: \`${analysis.source_commit_sha.slice(0, 12)}\`.`,
    `[Full report](https://github.com/${repository}/actions/runs/${run.run_id.split('-')[0]}) (artifact).`, '',
    '</details>', '',
  ].join('\n');
  // Reserve the final marker and metadata before dividing the remaining rendered text budget.
  const summaryLimit = Math.floor((PUBLICATION_BUDGET - publicationMarker(snapshot, analysis, review).length -
    1 - renderComment(0).length) / 2);
  check(summaryLimit >= 3, 'Publication metadata exceeds the summary budget');
  const comment = renderComment(summaryLimit);
  fs.writeFileSync(path.join(root, 'output/comment.md'), comment);
  if (authorized && isPublishable(analysis)) {
    plan.operations = [
      ...(approvedPriority && !existing.includes(approvedPriority) ? [{ action: 'add_label', label: approvedPriority }] : []),
      commentOperation(snapshot, comment),
    ];
  } else if (authorized) {
    plan.operations = [{ action: 'record_completion', input_fingerprint: snapshot.input_fingerprint }];
  }
  writeJson(path.join(root, 'output/writeback-plan.json'), plan);
  const runFile = path.join(root, 'output/run.json');
  writeJson(runFile, {
    ...readJson(runFile), stage: 'report_complete', finished_at_utc: new Date().toISOString(),
    gates: { G0: 'pass', G1: 'not_applicable', G2: analysis.scope.status === 'scope_unresolved' ? 'pending' : 'pass', G3: priority || refuted ? 'pass' : 'pending',
      G4: review.verdict === 'accept' ? 'pass' : 'pending', G5: authorized ? 'pending' : 'not_applicable', G6: authorized ? 'pending' : 'not_applicable' },
    completeness: 'One selected issue; source analysis and same-agent review. No dynamic reproduction, functional tests, full candidate discovery, or persistent incremental index.',
  });
  writeManifest(root);
  return report;
}

function writeManifest(root) {
  const output = path.join(root, 'output');
  fs.writeFileSync(path.join(output, 'manifest.sha256'), fs.readdirSync(output).sort()
    .filter((name) => name !== 'manifest.sha256')
    .map((name) => `${sha256(fs.readFileSync(path.join(output, name)))}  ${name}\n`).join(''));
}

function issueRevision(issue) {
  return JSON.stringify([issue.id, issue.title, issue.body, issue.state,
    issue.labels.map((label) => typeof label === 'string' ? label : label.name).sort()]);
}

async function readPublication({ github, context, env }) {
  check(env.PUBLISH_ALLOWED === 'true', 'Issue publication is not enabled');
  const output = path.join(env.ANALYSIS_ROOT, 'output');
  const { data: repository } = await github.rest.repos.get(context.repo);
  assertRepository(repository);
  const run = readJson(path.join(output, 'run.json'));
  assertRepository(run.repository);
  const producingRun = /^([1-9]\d*)-([1-9]\d*)$/.exec(run.run_id);
  const producingAttempt = Number(producingRun?.[2]);
  const publishingAttempt = Number(env.GITHUB_RUN_ATTEMPT || '1');
  // Re-running failed jobs can retain the successful analysis from an earlier attempt.
  check(typeof run.run_id === 'string' && producingRun?.[1] === String(context.runId) && Number.isSafeInteger(producingAttempt) &&
    Number.isSafeInteger(publishingAttempt) && producingAttempt <= publishingAttempt &&
    run.workflow_commit_sha === context.sha && run.permissions.publish_issue_results === true,
  'Artifact does not belong to this authorized workflow run');
  const snapshot = readJson(path.join(output, 'snapshot.json'));
  check(run.repository.id === repository.id && snapshot.repository.id === repository.id,
    'Artifact belongs to a different repository');
  const analysis = parseResult(fs.readFileSync(path.join(output, 'analysis.json'), 'utf8'));
  const review = parseResult(fs.readFileSync(path.join(output, 'review.json'), 'utf8'));
  validateReview(review, analysis);
  check(analysis.issue_number === snapshot.issue.number &&
    analysis.source_commit_sha === snapshot.target.source_commit_sha, 'Publication input mismatch');
  for (const phase of ['analysis', 'review']) {
    check(readJson(path.join(output, `${phase}-execution.json`)).validation === 'passed',
      'Only validated agent results can be published');
  }
  const reviewExecution = readJson(path.join(output, 'review-execution.json'));
  check(['inline_self_review', 'focused_model_review'].includes(reviewExecution.execution_kind), 'Missing review execution provenance');
  if (reviewExecution.execution_kind === 'inline_self_review') {
    check(reviewReasons(analysis).length === 0, 'This decision requires a focused model review');
    check(JSON.stringify(review) === JSON.stringify(inlineReview(analysis)), 'Inline review differs from the validated analysis');
  }
  const plan = readJson(path.join(output, 'writeback-plan.json'));
  const expectedPriority = review.verdict === 'accept' ? analysis.priority.label : null;
  check(plan.authorized === true && plan.suggested_priority === analysis.priority.label &&
    plan.approved_priority === expectedPriority && plan.repository.toLowerCase() === repository.full_name.toLowerCase(),
    'Publication plan does not match the reviewed decision');
  await authorizeActor(github, context);

  check(plan.component === snapshot.component.id && plan.issue_number === snapshot.issue.number &&
    run.issue_id === snapshot.issue.id && run.issue_number === snapshot.issue.number &&
    plan.source_commit_sha === snapshot.target.source_commit_sha, 'Publication component or issue mismatch');
  return { repository, run, snapshot, analysis, review, plan, expectedPriority };
}

async function publish({ github, context, core, env }) {
  check(env.PUBLISH_ALLOWED === 'true', 'Issue publication is not enabled');
  const root = env.ANALYSIS_ROOT;
  const output = path.join(root, 'output');
  const result = { status: 'pending', operations: [], started_at_utc: new Date().toISOString() };
  try {
    const { repository, snapshot, analysis, review, plan, expectedPriority } =
      await readPublication({ github, context, env });
    if (!isPublishable(analysis)) {
      check(JSON.stringify(plan.operations) === JSON.stringify([
        { action: 'record_completion', input_fingerprint: snapshot.input_fingerprint },
      ]), 'Completion operations do not match the reviewed snapshot');
      const completion = await recordCompletion({ github, context, snapshot, analysis });
      result.completion_comment_id = completion.id;
      result.operations.push({ action: 'record_completion', ...completion });
      result.status = 'skipped';
      result.reason = 'Only active in-scope or V1-only reports are published';
      return result;
    }
    const params = { ...context.repo, issue_number: snapshot.issue.number };
    const comment = fs.readFileSync(path.join(output, 'comment.md'), 'utf8');
    const marker = publicationMarker(snapshot, analysis, review);
    const operation = commentOperation(snapshot, comment);
    const originalNames = snapshot.issue.labels.map((label) => typeof label === 'string' ? label : label.name);
    const addPriority = expectedPriority && !originalNames.includes(expectedPriority);
    const publishedIssue = addPriority ? { ...snapshot.issue,
      labels: [...snapshot.issue.labels, { name: expectedPriority }] } : snapshot.issue;
    const comments = await github.paginate(github.rest.issues.listComments, { ...params, per_page: 100 });
    const previous = comments.find((item) => item.user?.login === 'github-actions[bot]' &&
      item.body === `${marker}\n${comment}`);
    if (previous) {
      const { data: current } = await github.rest.issues.get(params);
      check(!current.pull_request && issueRevision(current) === issueRevision(publishedIssue),
        'Previously published issue changed; rerun before publication');
      result.status = 'skipped';
      result.reason = 'Identical validated summary already published';
      result.comment_url = previous.html_url;
      return result;
    }
    const labels = await github.paginate(github.rest.issues.listLabelsForRepo, { ...context.repo, per_page: 100 });
    if (expectedPriority) {
      check(labels.some((label) => label.name === expectedPriority), 'Suggested label does not exist');
      check(JSON.stringify(priorityRules(labels)) === JSON.stringify(snapshot.rules),
        'Priority definitions changed during analysis; rerun before publication');
    }
    const { data: current } = await github.rest.issues.get(params);
    const names = current.labels.map((label) => label.name);
    const expectedOperations = [
      ...(addPriority ? [{ action: 'add_label', label: expectedPriority }] : []),
      operation,
    ];
    check(JSON.stringify(plan.operations) === JSON.stringify(expectedOperations),
      'Publication operations do not match the reviewed report and label delta');
    if (operation.action === 'update_comment') {
      const previousReport = comments.find((item) => item.id === operation.comment_id);
      check(previousReport?.user?.login === 'github-actions[bot]' &&
        sha256(previousReport.body) === operation.previous_body_sha256,
      'Planned publication comment changed; rerun before publication');
    }
    // A previous publication attempt may have added the label before its comment request failed.
    check(!current.pull_request && (issueRevision(current) === issueRevision(snapshot.issue) ||
      issueRevision(current) === issueRevision(publishedIssue)),
    'Issue changed during analysis; rerun before publication');
    const priorities = names.filter((name) => name.startsWith('priority:'));
    check(!expectedPriority || priorities.length === 0 ||
      (priorities.length === 1 && priorities[0] === expectedPriority),
    'An existing priority differs from the suggestion; maintainer review is required');
    const body = `${marker}\n${comment}`;
    check(body.length <= PUBLICATION_BUDGET, 'Summary exceeds the publication budget; use the artifact');
    if (expectedPriority && !names.includes(expectedPriority)) {
      await github.rest.issues.addLabels({ ...params, labels: [expectedPriority] });
      result.operations.push({ action: 'add_label', label: expectedPriority });
    }
    const { data: created } = operation.action === 'update_comment' ?
      await github.rest.issues.updateComment({ ...context.repo, comment_id: operation.comment_id, body }) :
      await github.rest.issues.createComment({ ...params, body });
    result.operations.push({ action: operation.action, id: created.id, body_sha256: sha256(body) });
    const { data: verifiedComment } = await github.rest.issues.getComment({ ...context.repo, comment_id: created.id });
    const { data: verifiedIssue } = await github.rest.issues.get(params);
    check(verifiedComment.body === body && !verifiedIssue.pull_request &&
      issueRevision(verifiedIssue) === issueRevision(publishedIssue), 'Publication readback did not match the plan');
    result.status = 'verified';
    result.comment_url = verifiedComment.html_url;
    core.info(`Published verified analysis: ${result.comment_url}`);
    return result;
  } catch (error) {
    result.status = 'failed';
    result.error = error.message;
    throw error;
  } finally {
    result.finished_at_utc = new Date().toISOString();
    writeJson(path.join(output, 'writeback-results.json'), result);
    const runFile = path.join(output, 'run.json');
    if (fs.existsSync(runFile)) {
      const run = readJson(runFile);
      writeJson(runFile, { ...run, publication_status: result.status, gates: { ...run.gates,
        G5: result.status === 'failed' ? 'blocked' : 'pass',
        G6: result.status === 'failed' ? 'blocked' : result.comment_url ? 'pass' : 'not_applicable' } });
    }
    writeManifest(root);
  }
}

module.exports = { main, hasCompletedAnalysis, completionPrefix, publicationMarker, readPublication, issueRevision, isPublishable, authorizeActor, componentForIssue, componentScope, inputFingerprint, commentPrefix, assertRepository, priorityRules, prepare, stageSource, parseResult,
  validateAnalysis, validateReview, inlineReview, reviewReasons, reviewIfNeeded, agentArguments, runAgent, reviewContext, revise, finish, publish, parseCliResult, phasePolicy };

function main() {
  try {
    const [command, root, argument] = process.argv.slice(2);
    check(root, 'Usage: issue-analysis.js <stage|analysis|review|review-if-needed|revise|finish> <run-root> [checkout|policy]');
    if (command === 'stage') stageSource(argument, root);
    else if (command === 'analysis' || command === 'review') runAgent(root, command, argument);
    else if (command === 'revise') revise(root, argument);
    else if (command === 'review-if-needed') {
      const reasons = reviewIfNeeded(root, argument);
      console.log(JSON.stringify({ extra_model_review: reasons.length > 0, reasons }));
    }
    else if (command === 'finish') finish(root);
    else throw new Error(`Unknown command: ${command}`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    const root = process.argv[3];
    if (root && fs.existsSync(path.join(root, 'output'))) writeManifest(root);
    process.exitCode = 1;
  }
}

if (require.main === module) main();
