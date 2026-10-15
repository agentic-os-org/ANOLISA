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

function invoke(api, handler, before, event, context, completion) {
  const config = api.pluginConfig;
  let input;
  try {
    input = JSON.stringify({
      hook: before ? 'before_tool_call' : 'after_tool_call', event, context,
    });
    if (completion) input = JSON.stringify({ native_input: input, aw_openclaw_completion: completion });
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

function ready(api, count, degraded) {
  const receipt = api.pluginConfig.ready;
  api.on('gateway_start', () => {
    const temporary = `${receipt.path}.${process.pid}.${randomUUID()}.tmp`;
    try {
      writeFileSync(temporary, JSON.stringify({
        version: 1, adapter: 'openclaw', token: receipt.token,
        pid: process.pid, hooks: count, ...(degraded ? { degraded } : {}),
      }) + '\n', { mode: 0o600, flag: 'wx' });
      renameSync(temporary, receipt.path);
    } finally {
      rmSync(temporary, { force: true });
    }
  });
}

export function register(api, sanitizeToolResult) {
  const config = api.pluginConfig;
  if (!config?.binary || !config.binding || !config.ready?.path || !config.ready.token) {
    throw new Error('AW requires a callback binary, binding and readiness receipt');
  }
  let count = 0;
  let degraded;
  for (const [group, hook] of [['before', 'before_tool_call'], ['after', 'after_tool_call']]) {
    const handlers = config.hooks[group];
    if (group === 'after' && handlers.some(handler => handler.replaceResult)) {
      const replacement = handlers.find(handler => handler.replaceResult);
      const observers = handlers.filter(handler => !handler.replaceResult);
      const snapshots = new Map();
      const key = (event, context) => JSON.stringify([context.sessionId, context.runId ?? event.runId, event.toolCallId]);
      if (typeof api.registerAgentToolResultMiddleware === 'function') {
        api.registerAgentToolResultMiddleware(async (event, context) => {
          try {
            const normalized = { ...event, params: event.args };
            const snapshot = JSON.stringify({ hook: 'after_tool_call', event: normalized, context });
            const completion = observers.length ? { phase: 'replacement', snapshot } : undefined;
            // Never compress a result whose original observation cannot be retained.
            // The callback frame carries both native views within the existing limit.
            if (observers.length && (snapshots.size >= 32 || Buffer.byteLength(JSON.stringify({
              native_input: snapshot, aw_openclaw_completion: completion,
            })) > 1024 * 1024)) {
              api.logger?.warn?.('AW compression skipped: completion snapshot limit');
              return undefined;
            }
            const original = observers.length ? sanitizeToolResult(event.result) : undefined;
            if (observers.length) snapshots.set(key(event, context), { snapshot, original });
            const reply = await invoke(api, replacement, false, normalized, context, completion);
            if (reply && Object.hasOwn(reply, 'aw_replacement')) return { result: reply.aw_replacement };
            return undefined;
          } catch (error) { return failure(api, replacement, false, error); }
        }, { runtimes: ['openclaw'] });
      } else {
        degraded = 'result_middleware_unavailable';
        api.logger?.warn?.('AW result replacement unavailable: OpenClaw tool-result middleware missing');
      }
      if (observers.length) {
        api.on(hook, async (event, context) => {
          const entry = snapshots.get(key(event, context));
          snapshots.delete(key(event, context));
          const original = entry ? { ...event, result: entry.original } : event;
          for (const handler of observers) {
            await invoke(api, handler, false, original, context,
              entry ? { phase: 'observation', snapshot: entry.snapshot } : undefined);
          }
        });
        // Completion can be suppressed by native cancellation or run teardown.
        api.on('session_end', (event, context) => {
          for (const saved of snapshots.keys()) {
            const [session] = JSON.parse(saved);
            if (session === (context.sessionId ?? event.sessionId)) snapshots.delete(saved);
          }
        });
        api.on('gateway_stop', () => snapshots.clear());
      }
      count += handlers.length;
      continue;
    }
    for (const handler of handlers) {
      // Without replacement, each step retains native scheduling.
      // Before hooks also retain native terminal block short-circuiting.
      if (handler.replaceResult) throw new Error('AW result replacement requires an after hook');
      api.on(hook, (event, context) => invoke(api, handler, group === 'before', event, context));
      count += 1;
    }
  }
  ready(api, count, degraded);
}
