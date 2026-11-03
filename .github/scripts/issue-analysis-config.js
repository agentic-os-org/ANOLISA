'use strict';

const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const COMPONENTS = require('../components.json');
const PROFILES = require('../issue-analysis/components.json');
const sha256 = (value) => crypto.createHash('sha256').update(value).digest('hex');
function check(condition, message) { if (!condition) throw new Error(message); }

function componentForIssue(issue) {
  const labels = issue.labels.map((label) => typeof label === 'string' ? label : label.name)
    .filter((name) => name.startsWith('component:'));
  if (labels.length !== 1) return { reason: labels.length ? 'ambiguous_component' : 'missing_component' };
  const component = COMPONENTS.components.find((item) => item.label === labels[0] && item.status !== 'internal');
  if (!component || !PROFILES[component.id]) return { reason: 'unsupported_component' };
  return { component };
}

function componentScope(component) {
  const profile = PROFILES[component.id];
  check(profile, 'Missing component analysis policy');
  const policy = fs.readFileSync(path.join(__dirname, '../issue-analysis/policy.md'), 'utf8');
  return { ...profile, policy_sha256: sha256(policy + JSON.stringify(profile)) };
}


function enabledComponents(raw = '') {
  if (!raw.trim()) return [];
  const enabled = [...new Set(raw.split(',').map((item) => item.trim()))];
  for (const id of enabled) {
    check(COMPONENTS.components.some((item) => item.id === id && item.status !== 'internal') &&
      Object.hasOwn(PROFILES, id), `ISSUE_ANALYSIS_COMPONENTS contains an unknown public component: ${id || '(empty)'}`);
  }
  return enabled;
}

function selectIssue(issue, enabled) {
  if (!enabled.length) return { reason: 'analysis_disabled' };
  if (issue.pull_request) return { reason: 'pull_request' };
  if (issue.state !== 'open') return { reason: 'closed_issue' };
  const selection = componentForIssue(issue);
  if (selection.reason) return selection;
  if (!enabled.includes(selection.component.id)) return { reason: 'component_disabled' };
  if (issue.labels.some((label) => (typeof label === 'string' ? label : label.name).startsWith('priority:'))) {
    return { reason: 'existing_priority_skipped' };
  }
  return selection;
}

module.exports = { componentForIssue, componentScope, enabledComponents, selectIssue };
