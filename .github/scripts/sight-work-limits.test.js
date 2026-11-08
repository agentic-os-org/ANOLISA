'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const { workLimit, quotaExempt } = require('./sight-work-limits');
const sightMaintainer = require('../maintainers.json').scopes
  .find(entry => entry.label === 'component:sight').maintainers[0].github;

test('external limits default to five and reject unsafe configuration', () => {
  for (const value of [undefined, '', ' ']) assert.equal(workLimit(value, 'LIMIT'), 5);
  for (const value of ['1', '5', '100']) assert.equal(workLimit(value, 'LIMIT'), Number(value));
  for (const value of ['0', '-1', '1.5', 'NaN', '101', 'Infinity', '01']) {
    assert.throws(() => workLimit(value, 'LIMIT'), /LIMIT must be an integer/);
  }
});

test('registered sight maintainers are exempt without a permission lookup', async () => {
  assert.equal(await quotaExempt({}, {}, sightMaintainer.toUpperCase()), true);
});

test('repository write permissions exempt authors, but read, triage and membership alone do not', async () => {
  for (const permission of ['write', 'maintain', 'admin', 'read', 'triage', 'none']) {
    const github = { rest: { repos: { getCollaboratorPermissionLevel: async (args) => {
      assert.deepEqual(args, { owner: 'org', repo: 'repo', username: 'author' });
      return { data: { permission } };
    } } } };
    assert.equal(await quotaExempt(github, { owner: 'org', repo: 'repo' }, 'author'),
      ['write', 'maintain', 'admin'].includes(permission));
  }
});

test('non-collaborators remain limited and permission outages cannot grant exemptions', async () => {
  for (const status of [404, 403, 429, 500]) {
    const github = { rest: { repos: { getCollaboratorPermissionLevel: async () => {
      throw Object.assign(new Error('lookup failed'), { status });
    } } } };
    if (status === 404) assert.equal(await quotaExempt(github, {}, 'external'), false);
    else await assert.rejects(quotaExempt(github, {}, 'author'), /Cannot verify.*retry/);
  }
});
