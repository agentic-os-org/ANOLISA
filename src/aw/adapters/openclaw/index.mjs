import { execFile } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { renameSync, rmSync, writeFileSync } from 'node:fs';

const MAX_OUTPUT_BYTES = 1024 * 1024 + 64 * 1024;

function failure(api, handler, before, error) {
  api.logger?.warn?.(`AW ${before ? 'before' : 'after'} tool hook failed: ${error.message}`);
  return before && handler.onError === 'block'
    ? { block: true, blockReason: 'AW tool policy could not be evaluated' }
    : undefined;
}

function invoke(api, handler, before, event, context) {
  const config = api.pluginConfig;
  let input;
  try {
    input = JSON.stringify({
      hook: before ? 'before_tool_call' : 'after_tool_call', event, context,
    });
  } catch (error) {
    return Promise.resolve(failure(api, handler, before, error));
  }
  return new Promise((resolve) => {
    let inputError;
    const child = execFile(config.binary, [
      'hook', '--adapter', 'openclaw', '--binding', config.binding,
      '--event', before ? 'tool.before' : 'tool.after',
      '--step', handler.step, '--on-error', handler.onError,
    ], {
      encoding: 'utf8', timeout: handler.budgetMs + 2500,
      maxBuffer: MAX_OUTPUT_BYTES,
      // Native commands retain the host's callback-time environment; structured
      // Providers still use the separate launch-time context pinned in AW.
      env: process.env,
    }, (error, stdout, stderr) => {
      if (stderr) process.stderr.write(stderr);
      if (error || inputError) {
        resolve(failure(api, handler, before, inputError ?? error));
        return;
      }
      try {
        const value = stdout.trim() ? JSON.parse(stdout) : undefined;
        if (value !== undefined && (!value || typeof value !== 'object' || Array.isArray(value))) {
          throw new Error('AW native hook output must be a JSON object or empty');
        }
        resolve(value);
      } catch (error) {
        resolve(failure(api, handler, before, error));
      }
    });
    child.stdin.on('error', error => {
      inputError = error;
      child.kill();
    });
    child.stdin.end(input);
  });
}

function ready(api, count) {
  const receipt = api.pluginConfig.ready;
  api.on('gateway_start', () => {
    const temporary = `${receipt.path}.${process.pid}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporary, JSON.stringify({
        version: 1, adapter: 'openclaw', token: receipt.token,
        pid: process.pid, hooks: count,
      }) + '\n', { mode: 0o600, flag: 'wx' });
      renameSync(temporary, receipt.path);
    } finally {
      rmSync(temporary, { force: true });
    }
  });
}

export default {
  id: 'aw-native-hooks',
  name: 'AW native tool hooks',
  register(api) {
    const config = api.pluginConfig;
    if (!config?.binary || !config.binding || !config.ready?.path || !config.ready.token) {
      throw new Error('AW requires a callback binary, binding and readiness receipt');
    }
    let count = 0;
    for (const [group, hook] of [['before', 'before_tool_call'], ['after', 'after_tool_call']]) {
      for (const handler of config.hooks[group]) {
        // Register each step separately: OpenClaw owns before serial ordering,
        // terminal block short-circuiting, and after parallel execution.
        api.on(hook, (event, context) => invoke(api, handler, group === 'before', event, context));
        count += 1;
      }
    }
    ready(api, count);
  },
};
