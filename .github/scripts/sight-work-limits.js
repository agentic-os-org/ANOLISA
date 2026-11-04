'use strict';

const maintainers = require('../maintainers.json');

function workLimit(value, name) {
  const text = String(value ?? '').trim();
  if (!text) return 5;
  if (!/^[1-9]\d*$/.test(text) || Number(text) > 100) {
    throw new Error(`${name} must be an integer from 1 to 100.`);
  }
  return Number(text);
}

async function quotaExempt(github, repository, login) {
  const scope = maintainers.scopes.find((entry) => entry.label === 'component:sight');
  if (scope?.maintainers.some((entry) => entry.github.toLowerCase() === login.toLowerCase())) {
    return true;
  }
  try {
    const { data } = await github.rest.repos.getCollaboratorPermissionLevel({
      ...repository, username: login,
    });
    return ['write', 'maintain', 'admin'].includes(data.permission);
  } catch (error) {
    if (error.status === 404) return false;
    throw new Error(`Cannot verify AgentSight quota exemption for @${login}; retry the check.`, { cause: error });
  }
}

module.exports = { workLimit, quotaExempt };
