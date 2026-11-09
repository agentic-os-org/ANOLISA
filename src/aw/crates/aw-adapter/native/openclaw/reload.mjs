/** Keep a native run busy until its observed tool completions have drained. */
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';

// Keep every admitted identity for this binding's lifetime. Exhaustion rejects
// new runs rather than evicting identities which may still have late callbacks.
const MAX_IDENTITIES = 4096;

const denied = () => ({ block: true, blockReason: 'AW native run admission is unavailable' });
const key = (event, context) => {
  const values = [context.sessionId, context.runId ?? event.runId];
  if (values.some(value => typeof value !== 'string' || !value || Buffer.byteLength(value) > 512))
    throw new Error('AW requires bounded native session and run identities');
  return createHash('sha256').update(JSON.stringify(values)).digest('hex');
};

function lifecycle(config, phase, identity = '') {
  return new Promise((resolve, reject) => {
    const child = execFile(config.binary, ['bridge', '--adapter', 'openclaw', '--binding', config.binding],
      { encoding: 'utf8', timeout: 5000, maxBuffer: 65536 }, error => {
        if (error) reject(new Error(`AW native ${phase} failed`)); else resolve();
      });
    child.stdin.on('error', reject);
    child.stdin.end(JSON.stringify({ phase, key: identity, pid: process.pid }));
  });
}

export function registerReload(api, invoke) {
  const config = api.pluginConfig;
  // OpenClaw loads separate registry views for Gateway and embedded agents.
  // Preserve admission across those views and native plugin reloads.
  const slot = Symbol.for('aw.openclaw.reload.v1');
  const states = globalThis[slot] ??= new Map();
  if (!states.has(config.binding)) {
    if (states.size >= 16) throw new Error('AW native binding capacity exceeded');
    states.set(config.binding, { runs: new Map(), completed: new Set(), ready: false });
  }
  const shared = states.get(config.binding);
  const runs = shared.runs;
  const before = { step: '$aw.dispatch', onError: 'block', budgetMs: 12000 };
  const after = { step: '$aw.dispatch', onError: 'report', budgetMs: 12000 };
  const find = (event, context) => {
    const state = runs.get(key(event, context));
    if (!shared.ready || !state?.admitted || state.ended || state.failed)
      throw new Error('AW native run is not admitted');
    return state;
  };
  const track = (state, callback) => {
    const work = Promise.resolve().then(callback);
    state.callbacks.add(work);
    return work.finally(() => state.callbacks.delete(work));
  };
  api.on('before_agent_run', async (event, context) => {
    let state;
    try {
      const identity = key(event, context);
      if (!shared.ready) throw new Error('AW Gateway is not ready');
      if (runs.has(identity) || shared.completed.has(identity)) {
        const previous = runs.get(identity);
        if (previous) previous.failed = true;
        throw new Error('AW native run identity was reused');
      }
      if (runs.size >= 128 || runs.size + shared.completed.size >= MAX_IDENTITIES)
        throw new Error('AW native run identity capacity exceeded');
      state = { admitted: false, ended: false, failed: false, callbacks: new Set(), tools: new Map() };
      runs.set(identity, state);
      await lifecycle(config, 'request_start', identity);
      state.admitted = true;
      return { outcome: 'pass' };
    } catch (error) {
      if (state) state.failed = true;
      api.logger?.warn?.(`AW run preparation failed: ${error.message}`);
      return { outcome: 'block', reason: 'AW native run admission failed' };
    }
  });
  api.on('before_tool_call', async (event, context) => {
    let state;
    try {
      state = find(event, context);
      const call = event.toolCallId ?? context.toolCallId;
      if (typeof call !== 'string' || !call || Buffer.byteLength(call) > 512 ||
          state.tools.has(call) || state.tools.size >= 128) throw new Error('AW invalid tool identity');
      const tool = { result: false };
      state.tools.set(call, tool);
      return await track(state, async () => {
        const result = await invoke(api, before, true, event, context);
        return result;
      });
    } catch {
      if (state) state.failed = true;
      return denied();
    }
  });
  if (typeof api.registerAgentToolResultMiddleware !== 'function')
    throw new Error('AW reloadable Gateway requires native tool-result middleware');
  api.registerAgentToolResultMiddleware(async (event, context) => {
    let state;
    try {
      state = find(event, context);
      const tool = state.tools.get(event.toolCallId ?? context.toolCallId);
      if (!tool || tool.result) throw new Error('AW unexpected native tool result');
      return await track(state, async () => {
        const reply = await invoke(api, after, false, { ...event, params: event.args }, context);
        tool.result = true;
        return reply && Object.hasOwn(reply, 'aw_replacement') ? { result: reply.aw_replacement } : undefined;
      });
    } catch {
      if (state) state.failed = true;
      api.logger?.warn?.('AW tool completion failed; policy updates remain busy');
      return undefined;
    }
  }, { runtimes: ['openclaw'] });
  api.on('after_tool_call', async (event, context) => {
    let state;
    try {
      state = find(event, context);
      const call = event.toolCallId ?? context.toolCallId;
      const tool = state.tools.get(call);
      // Native blocked/error completions bypass result middleware. They still
      // need after observation and confirmed completion before idle admission.
      if (!tool?.result) {
        await track(state, async () => {
          await invoke(api, after, false, event, context);
          state.tools.delete(call);
        });
      } else state.tools.delete(call);
    } catch {
      if (state) state.failed = true;
    }
  });
  api.on('agent_end', async (event, context) => {
    try {
      const identity = key(event, context);
      const state = runs.get(identity);
      if (!state || state.ended) return;
      state.ended = true;
      await Promise.all([...state.callbacks]);
      // Error and cancellation paths do not prove native background cleanup.
      if (event.success !== true || state.failed || state.tools.size || !state.admitted) {
        api.logger?.warn?.(`AW run remains busy: success=${event.success === true} failed=${state.failed} tools=${state.tools.size}`);
        return;
      }
      await lifecycle(config, 'request_end', identity);
      shared.completed.add(identity);
      runs.delete(identity);
    } catch { api.logger?.warn?.('AW run cleanup is uncertain; policy updates remain busy'); }
  });
  return async () => {
    await lifecycle(config, 'ready');
    shared.ready = true;
  };
}
