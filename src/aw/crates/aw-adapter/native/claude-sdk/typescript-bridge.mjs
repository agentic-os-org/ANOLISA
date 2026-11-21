/** One foreground Query using the public SDK and the shared native CLI gate. */
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, renameSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { run } from './shared/transport.mjs';

let created = false;
const copy = value => JSON.parse(JSON.stringify(value));
const deny = reason => ({ hookSpecificOutput: { hookEventName: 'PreToolUse',
  permissionDecision: 'deny', permissionDecisionReason: reason } });

export async function createQuery({ prompt, options }) {
  if (created) throw Error('AW Claude SDK supports one foreground Query');
  const config = JSON.parse(readFileSync(process.env.AW_CLAUDE_CONFIG, 'utf8'));
  const finalEnforcing = Boolean(config.final_profile) && config.final_enforcing !== false;
  const root = dirname(config.ready_path);
  const allowed = ['model', 'cwd', 'settingSources', 'tools', 'allowedTools', 'mcpServers',
    'hooks', 'env', 'maxTurns'];
  if (typeof prompt !== 'string' || !options || Object.keys(options).some(key => !allowed.includes(key)) ||
      typeof options.model !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]+$/.test(options.model) ||
      ['auto', 'default', 'sonnet', 'opus', 'haiku'].includes(options.model) ||
      JSON.stringify(options.settingSources) !== '[]' || !Array.isArray(options.tools) ||
      options.tools.some(tool => tool !== 'Bash')) {
    throw Error('AW Claude TypeScript requires explicit foreground model, tools and settingSources=[]');
  }
  const servers = options.mcpServers ?? {};
  if (!servers || typeof servers !== 'object' || Array.isArray(servers) ||
      Object.values(servers).some(server => server?.type !== 'sdk')) {
    throw Error('AW Claude TypeScript covers only in-process SDK MCP servers');
  }
  const prefixes = Object.keys(servers).map(name => `mcp__${name}__`);
  const supportsTool = payload => typeof payload.tool_name === 'string' &&
    (payload.tool_name === 'Bash' || prefixes.some(prefix => payload.tool_name.startsWith(prefix))) &&
    !payload.tool_input?.run_in_background;
  for (const [name, file] of Object.entries(config.typescript.files)) {
    if (createHash('sha256').update(readFileSync(file)).digest('hex') !== config.typescript.sha256[name]) {
      throw Error('AW Claude TypeScript fingerprint changed after admission');
    }
  }
  const env = { ...process.env, ...options.env };
  if (env.BUN_OPTIONS || env.NODE_OPTIONS || Object.keys(env).some(key =>
    key.startsWith('CLAUDE_CODE_USE_') && env[key])) throw Error('AW runtime/provider overrides unsupported');
  let records = Object.create(null);
  let snapshots;
  if (config.final_profile) {
    const base = new URL(env.ANTHROPIC_BASE_URL ?? 'https://api.anthropic.com');
    if (!['http:', 'https:'].includes(base.protocol) || base.username || base.password ||
        !/^[A-Za-z0-9/_-]*$/.test(base.pathname) || base.search || base.hash) {
      throw Error('AW final gate requires an HTTP(S) Messages base URL without credentials or query');
    }
    const preload = `${root}/claude-final-preload.mjs`;
    if (/\s/.test(preload)) throw Error('AW final preload path must not contain whitespace');
    writeFileSync(`${root}/claude-final-runtime.json`, JSON.stringify({ ...config,
      origin: base.origin, messages_path: base.pathname.replace(/\/+$/, '') + '/v1/messages',
      cwd: resolve(options.cwd ?? process.cwd()) }), { mode: 0o600 });
    env.BUN_OPTIONS = `--preload=${preload}`;
    env.AW_CLAUDE_FINAL_CONFIG = `${root}/claude-final-runtime.json`;
  }
  if (finalEnforcing) env.CLAUDE_CODE_DISABLE_BACKGROUND_TASKS = '1';
  const invoke = async (route, payload, signal, close = false) => {
    const result = await run(config.executable, ['hook', '--adapter', 'claude-sdk',
      '--binding', config.binding, '--event', route.event, '--step', route.step,
      '--on-error', route.on_error, ...(close ? ['--close-event', 'true'] : ['--event-receipt', 'true'])],
      Buffer.from(JSON.stringify(payload)), close ? 2500 : route.budget_ms + 3000, close ? undefined : signal);
    if (close) {
      if (result.code !== 0 || result.stdout.toString().trim()) throw Error('AW event cleanup failed');
      return {};
    }
    return result;
  };
  const callback = async (payload, call, context) => {
    const before = payload.hook_event_name === 'PreToolUse';
    if (call !== payload.tool_use_id) return before ? deny('AW tool identity mismatch') : {};
    if (before && finalEnforcing && !supportsTool(payload)) {
      return deny('AW final gate does not cover this tool path');
    }
    if (!before && config.final_profile && (finalEnforcing || supportsTool(payload))) {
      try {
        if (typeof call !== 'string' || !call) throw Error('AW final identity limit');
        const duplicate = Object.hasOwn(records, call);
        if (Object.keys(records).length >= 1024 && !(finalEnforcing && duplicate)) {
          throw Error('AW final identity limit');
        }
        const native = finalEnforcing && duplicate ? records[call] :
          copy(Object.fromEntries(['session_id', 'cwd', 'tool_use_id', 'tool_name', 'tool_input']
            .map(key => [key, payload[key]])));
        const next = { ...records, [call]: duplicate ? { ...native, ambiguous: true } : native };
        if (finalEnforcing) records = next;
        if (snapshots.publish(next)) records = next;
      } catch (error) {
        if (finalEnforcing) throw error;
      }
    }
    let active;
    let completed = false;
    try {
      for (const route of config.hooks.filter(route => route.event === (before ? 'tool.before' : 'tool.after'))) {
        context.signal?.throwIfAborted();
        active = route;
        try {
          const result = await invoke(route, payload, context.signal);
          const receipt = JSON.parse(result.stdout.toString());
          if (typeof receipt.event_complete !== 'boolean' || Object.keys(receipt).length !== 1) {
            throw Error('AW invalid event receipt');
          }
          if (receipt.event_complete) active = undefined;
          context.signal?.throwIfAborted();
          if (result.code === 2 && before) return deny('Tool blocked by AW policy');
          if (result.code !== 0) throw Error('AW callback failed');
        } catch (error) {
          context.signal?.throwIfAborted();
          process.stderr.write(`AW ${route.event}/${route.step} failed: ${error.message}\n`);
          if (before && route.on_error === 'block') return deny('AW callback failed closed');
        }
      }
      completed = true;
    } finally {
      // Cancellation must not cancel the separate bounded event cleanup.
      if (active && !completed) await invoke(active, payload, undefined, true);
    }
    return {};
  };
  const hooks = {};
  const completions = new Map();
  const completion = (event, call) => {
    const key = `${event}:${call}`;
    if (!completions.has(key)) {
      if (completions.size >= 3072) throw Error('AW callback identity limit');
      let resolve;
      const promise = new Promise(done => { resolve = done; });
      completions.set(key, { promise, resolve });
    }
    return completions.get(key);
  };
  for (const event of ['PreToolUse', 'PostToolUse', 'PostToolUseFailure']) {
    const routes = config.hooks.filter(route => route.event === (event === 'PreToolUse' ? 'tool.before' : 'tool.after'));
    hooks[event] = [{ onFailure: event === 'PreToolUse' &&
      (finalEnforcing || routes.some(route => route.on_error === 'block')) ? 'block' : 'continue',
    timeout: Math.ceil((routes.reduce((total, route) => total + route.budget_ms + 3000, 0) + 2500) / 1000),
    hooks: [async (payload, call, context) => {
      const finished = completion(event, call);
      try { return await callback(payload, call, context); }
      finally { finished.resolve(); }
    }] }];
    for (const matcher of options.hooks?.[event] ?? []) {
      hooks[event].push({ ...matcher, hooks: matcher.hooks.map(handler => async (payload, call, context) => {
        await completion(event, call).promise;
        const result = await handler(copy(payload), call, context);
        const specific = result?.hookSpecificOutput;
        if (event === 'PreToolUse' && specific?.updatedInput) return deny('AW does not cover argument rewrites');
        if (finalEnforcing && (specific?.additionalContext || result?.systemMessage)) {
          throw Error('AW final gate does not cover Hook context injection');
        }
        return result;
      }) });
    }
  }
  if (Object.keys(options.hooks ?? {}).some(event => !Object.hasOwn(hooks, event))) {
    throw Error('AW Claude TypeScript supports only tool Hooks');
  }
  const composed = { ...options, tools: [...options.tools], settingSources: [],
    ...(options.allowedTools ? { allowedTools: [...options.allowedTools] } : {}),
    mcpServers: { ...servers }, env, hooks, pathToClaudeCodeExecutable: config.typescript.binary };
  // A launch attempt consumes this factory, including import/initialization failure.
  created = true;
  const sdk = await import(pathToFileURL(config.typescript.entry));
  let release;
  const ready = new Promise(resolve => { release = resolve; });
  async function* input() {
    await ready;
    yield { type: 'user', session_id: '', parent_tool_use_id: null,
      message: { role: 'user', content: prompt } };
  }
  let query;
  try {
    if (config.final_profile) {
      const { finalRecords } = await import('./final-records.mjs');
      snapshots = finalRecords(root, 'claude', finalEnforcing);
      snapshots.publish(records);
    }
    query = sdk.query({ prompt: input(), options: composed });
    await query.initializationResult();
    if (config.final_profile) {
      const runtime = JSON.parse(readFileSync(`${root}/claude-final-ready.json`, 'utf8'));
      if (runtime.token !== config.ready_token || JSON.stringify(runtime.profile) !== JSON.stringify(config.final_profile)) {
        throw Error('AW final runtime readiness mismatch');
      }
    }
    writeFileSync(`${config.ready_path}.tmp`, JSON.stringify({ version: 1, adapter: 'claude-sdk',
      pid: process.pid, token: config.ready_token, hooks: config.hooks.length }), { mode: 0o600 });
    renameSync(`${config.ready_path}.tmp`, config.ready_path);
    release();
    // Expose iteration and teardown only: runtime reconfiguration would invalidate admission.
    return { [Symbol.asyncIterator]: () => query[Symbol.asyncIterator](), close: () => {
      snapshots?.close();
      return query.close();
    } };
  } catch (error) {
    snapshots?.close();
    query?.close();
    release();
    throw error;
  }
}
