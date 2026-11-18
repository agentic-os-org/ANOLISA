import { registerReload } from './reload.mjs';
import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { renameSync, rmSync, writeFileSync } from 'node:fs';

const MAX_OUTPUT_BYTES = 1024 * 1024 + 64 * 1024;

function failure(api, handler, phase, error) {
  api.logger?.warn?.(`AW ${phase} tool hook failed: ${error.message}`);
  return phase !== 'after' && handler.onError === 'block'
    ? { block: true, blockReason: 'AW tool policy could not be evaluated' }
    : undefined;
}

function invokeEvent(api, handler, phase, event, context, completion, signal, close = false) {
  const config = api.pluginConfig;
  const awEvent = phase === 'final' ? 'security.violation' : `tool.${phase}`;
  let input;
  try {
    input = JSON.stringify({
      hook: phase === 'before' ? 'before_tool_call' : phase === 'after' ? 'after_tool_call' : 'final_tool_result',
      event, context,
    });
    if (completion) input = JSON.stringify({ native_input: input, aw_openclaw_completion: completion });
  } catch (error) {
    return Promise.resolve(failure(api, handler, phase, error));
  }
  return new Promise((resolve) => {
    let inputError;
    let reply;
    const child = execFile(config.binary, [
      'hook', '--adapter', 'openclaw', '--binding', config.binding,
      '--event', awEvent,
      '--step', handler.step, '--on-error', handler.onError,
      ...(close ? ['--close-event', 'true'] : []),
    ], {
      encoding: 'utf8', timeout: close ? 2500 : handler.budgetMs + 2500,
      signal, killSignal: 'SIGKILL',
      maxBuffer: MAX_OUTPUT_BYTES,
      // Native commands retain the host's callback-time environment; structured
      // Providers still use the separate launch-time context pinned in AW.
      env: process.env,
    }, (error, stdout, stderr) => {
      if (stderr) process.stderr.write(stderr);
      if (error || inputError) {
        reply = failure(api, handler, phase, inputError ?? error);
        return;
      }
      try {
        const value = stdout.trim() ? JSON.parse(stdout) : undefined;
        if (value !== undefined && (!value || typeof value !== 'object' || Array.isArray(value))) {
          throw new Error('AW native hook output must be a JSON object or empty');
        }
        reply = value;
      } catch (error) {
        reply = failure(api, handler, phase, error);
      }
    });
    child.once('close', () => resolve(reply));
    child.stdin.on('error', error => {
      inputError = error;
      child.kill('SIGKILL');
    });
    child.stdin.end(input);
  });
}

function invoke(api, handler, before, event, context, completion) {
  return invokeEvent(api, handler, before ? 'before' : 'after', event, context, completion);
}

function registerFinalGate(api, handlers) {
  if (!handlers.length) return;
  if (typeof api.registerAgentToolResultMiddleware !== 'function') {
    throw Error('AW final result gate requires OpenClaw result middleware');
  }
  const modelGate = Symbol.for('aw.openclaw.modelGate');
  const patched = Symbol.for('aw.openclaw.finalPatched');
  const payloadGate = Symbol.for('aw.openclaw.payloadGate');
  const providerPatched = Symbol.for('aw.openclaw.providerPatched');
  // OpenClaw registers the plugin twice during Gateway startup. Both native
  // registries must bind the same result identities to the one Agent core gate.
  const pendingKey = Symbol.for('aw.openclaw.finalPending');
  const pending = globalThis[pendingKey] ??= new Map();
  const receiptsKey = Symbol.for('aw.openclaw.finalReceipts');
  const receipts = globalThis[receiptsKey] ??= new Map();
  const pendingKeyFor = (session, call) => JSON.stringify([session, call]);
  // shortcut: only the pinned punctuation rewrite is mapped; other rewrites need a new profile.
  const normalizedCall = id => id.replace(/[^a-zA-Z0-9]/g, '');
  const withheld = 'AW withheld the final tool result';
  const supported = result => Array.isArray(result?.content) && result.content.every(
    block => block?.type === 'text' && typeof block.text === 'string');
  const check = async (record, recordKey, id, toolName, result, signal) => {
    if (!record || record.ambiguous || !supported(result)) return false;
    const { event, context } = record;
    const nativeEvent = {
      phase: 'model', toolName, toolCallId: id, runId: context.runId,
      params: event.args ?? {}, result,
    };
    const nativeContext = { sessionId: context.sessionId, runId: context.runId };
    const cancellation = signal ? AbortSignal.any([signal, record.controller.signal]) : record.controller.signal;
    const live = () => !cancellation.aborted && pending.get(recordKey) === record;
    let active;
    let completed = false;
    try {
      for (const handler of handlers) {
        if (!live()) return false;
        active = handler;
        const reply = await invokeEvent(api, handler, 'final', nativeEvent, nativeContext, undefined, cancellation);
        if (!live() || !reply || reply.block === true) return false;
      }
      completed = true;
      return true;
    } finally {
      if (active && !completed) {
        await invokeEvent(api, active, 'final', nativeEvent, nativeContext, undefined, undefined, true);
      }
    }
  };
  api.registerAgentToolResultMiddleware((event, context) => {
    const id = event.toolCallId;
    if (typeof id !== 'string' || !id || !context.sessionId || !context.runId) return;
    // A repeated in-flight ID cannot be bound to one final message.
    const identity = pendingKeyFor(context.sessionId, id);
    receipts.delete(identity);
    receipts.delete(pendingKeyFor(context.sessionId, normalizedCall(id)));
    const previous = pending.get(identity);
    previous?.controller.abort();
    if (previous) pending.set(identity, { ...structuredClone({ event, context }), ambiguous: true, controller: new AbortController() });
    else if (pending.size >= 128) return;
    else pending.set(identity, { ...structuredClone({ event, context }), controller: new AbortController() });
  }, { runtimes: ['openclaw'] });
  globalThis[modelGate] = async (input, config, signal, normalize) => {
    const sourceMessages = structuredClone(input);
    // Transcript-only outputs have no native call identity at this boundary.
    const requireRoles = rows => {
      if (!Array.isArray(rows) || rows.some(row => !['user', 'assistant', 'toolResult'].includes(row?.role))) {
        throw Error('AW model result checkpoint cannot verify a transcript output');
      }
    };
    requireRoles(sourceMessages);
    const transformed = structuredClone(config.transformContext
      ? await config.transformContext(structuredClone(sourceMessages), signal) : sourceMessages);
    requireRoles(transformed);
    const messages = structuredClone(await config.convertToLlm(normalize(structuredClone(transformed))));
    const withoutTools = await config.convertToLlm(normalize(structuredClone(
      transformed.filter(message => message.role !== 'toolResult'))));
    const sessionId = config.sessionId;
    const modelApi = config.model.api;
    if (!globalThis[patched] || !globalThis[providerPatched] ||
        !globalThis[Symbol.for('aw.openclaw.streamPatched')] ||
        modelApi !== 'openai-completions' || !sessionId ||
        !Array.isArray(messages) || !Array.isArray(sourceMessages) ||
        !Array.isArray(transformed) || !Array.isArray(withoutTools)) {
      throw Error('AW model result checkpoint is unavailable');
    }
    if (withoutTools.some(message => message.role === 'toolResult') ||
        JSON.stringify(messages.filter(message => message.role !== 'toolResult')) !==
        JSON.stringify(withoutTools)) {
      throw Error('AW result conversion changed a non-tool model message');
    }
    // The verified converter filters messages; transformed user or assistant
    // content must still come from the original context in the same order.
    const original = sourceMessages.filter(message => message.role !== 'toolResult');
    let cursor = 0;
    for (const message of transformed.filter(item => item.role !== 'toolResult')) {
      const snapshot = JSON.stringify(message);
      while (cursor < original.length && JSON.stringify(original[cursor]) !== snapshot) cursor++;
      if (cursor === original.length) throw Error('AW context transform changed a model-visible message');
      cursor++;
    }
    const projected = messages.filter(message => message.role === 'toolResult');
    for (const source of sourceMessages.filter(message => message.role === 'toolResult')) {
      if (!projected.some(message => message.toolCallId === source.toolCallId && message.toolName === source.toolName)) {
        throw Error('AW model result projection changed a tool result identity');
      }
    }
    if (projected.some(message => !supported(message) || !message.toolCallId)) {
      throw Error('AW model result projection contains an unsupported result');
    }
    return messages;
  };
  globalThis[payloadGate] = async (params, sessionId, beforeParams, sourceMessages, modelApi, encodeBody, signal) => {
    // The transport receives this owned snapshot after asynchronous policy checks.
    [params, beforeParams, sourceMessages] = structuredClone([params, beforeParams, sourceMessages]);
    if (encodeBody) {
      const encoded = await encodeBody(params);
      if (!(encoded.body instanceof Uint8Array)) throw Error('AW provider body encoding is unsupported');
      params = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(encoded.body));
    }
    const beforeMessages = beforeParams?.messages;
    if (!globalThis[patched] || !globalThis[providerPatched] ||
        !globalThis[Symbol.for('aw.openclaw.streamPatched')] ||
        modelApi !== 'openai-completions' || !sessionId ||
        !Array.isArray(params?.messages) || !Array.isArray(beforeMessages) ||
        !Array.isArray(sourceMessages)) {
      throw Error('AW provider payload checkpoint is unavailable');
    }
    const toolMessages = rows => rows.filter(message => message.role === 'tool');
    const otherMessages = rows => rows.filter(message => message.role !== 'tool');
    if (JSON.stringify({ ...params, messages: null }) !==
        JSON.stringify({ ...beforeParams, messages: null })) {
      throw Error('AW provider request changed non-message payload fields');
    }
    const sourceResults = sourceMessages.filter(message => message.role === 'toolResult');
    const toolShape = message => ({ ...message, content: null });
    if (JSON.stringify(otherMessages(params.messages)) !== JSON.stringify(otherMessages(beforeMessages)) ||
        JSON.stringify(toolMessages(params.messages).map(toolShape)) !==
        JSON.stringify(toolMessages(beforeMessages).map(toolShape)) ||
        toolMessages(params.messages).length !== sourceResults.length) {
      throw Error('AW provider request changed a model message identity');
    }
    const seen = new Set();
    const checked = [];
    let sourceIndex = 0;
    for (const message of params.messages) {
      if (message.role !== 'tool') { checked.push(() => message); continue; }
      const id = message.tool_call_id;
      const source = sourceResults[sourceIndex++];
      if (typeof id !== 'string' || !id || seen.has(id) ||
          normalizedCall(source.toolCallId) !== normalizedCall(id)) {
        throw Error('AW provider request has an ambiguous tool result');
      }
      seen.add(id);
      const identity = pendingKeyFor(sessionId, source.toolCallId);
      const candidates = pending.has(identity) ? [identity] : [...pending].filter(([, value]) =>
        value.context.sessionId === sessionId && value.event?.toolName === source.toolName &&
        normalizedCall(value.event.toolCallId) === source.toolCallId).map(([key]) => key);
      const recordKey = candidates.length === 1 ? candidates[0] : undefined;
      const record = recordKey ? pending.get(recordKey) : undefined;
      const digest = createHash('sha256').update(JSON.stringify(message.content)).digest('hex');
      const receipt = receipts.get(identity);
      const cached = !record && receipt?.digest === digest &&
        receipt.toolName === source.toolName && receipt.isError === (source.isError === true);
      let allow = cached;
      if (!allow && record && !record.ambiguous &&
          record.event.toolName === source.toolName && typeof message.content === 'string') {
        try {
          allow = await check(record, recordKey, source.toolCallId, source.toolName, {
            content: [{ type: 'text', text: message.content }],
            isError: source.isError === true,
          }, signal);
        } catch (error) {
          api.logger?.warn?.(`AW provider payload check failed: ${error.message}`);
        }
      }
      const collisions = candidates.map(key => [key, pending.get(key)]);
      checked.push(() => {
        // A later check can yield to teardown or a new native invocation.
        const current = record && pending.get(recordKey) === record;
        allow &&= cached ? receipts.get(identity) === receipt : current;
        if (current) pending.delete(recordKey);
        if (candidates.length > 1) for (const [key, value] of collisions) {
          if (pending.get(key) === value) pending.delete(key);
        }
        const delivered = allow ? message : { ...message, content: withheld };
        if (current) {
          if (receipts.size >= 1024) receipts.delete(receipts.keys().next().value);
          receipts.set(identity, {
            digest: createHash('sha256').update(JSON.stringify(delivered.content)).digest('hex'),
            toolName: source.toolName, isError: source.isError === true,
          });
        }
        return delivered;
      });
    }
    return { ...params, messages: checked.map(deliver => deliver()) };
  };
  api.on('session_end', (event, context) => {
    const sessionId = context.sessionId ?? event.sessionId;
    for (const [id, record] of pending) {
      if (record?.context.sessionId === sessionId) {
        record.controller.abort();
        pending.delete(id);
      }
    }
    for (const id of receipts.keys()) {
      if (JSON.parse(id)[0] === sessionId) receipts.delete(id);
    }
  });
  api.on('gateway_stop', () => {
    for (const record of pending.values()) record.controller.abort();
    pending.clear();
    receipts.clear();
    globalThis[modelGate] = async () => { throw Error('AW model result gate was disposed'); };
    globalThis[payloadGate] = async () => { throw Error('AW provider payload gate was disposed'); };
  });
}

function ready(api, count, degraded, admit) {
  const receipt = api.pluginConfig.ready;
  const publish = () => {
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
  };
  api.on('gateway_start', admit ? async () => { await admit(); publish(); } : publish);
}

export function register(api, sanitizeToolResult) {
  const config = api.pluginConfig;
  if (!config?.binary || !config.binding || !config.ready?.path || !config.ready.token) {
    throw new Error('AW requires a callback binary, binding and readiness receipt');
  }
  if (config.reloadable) {
    if (config.hooks.final?.length) throw Error('AW final result gate does not cover reloadable Gateway');
    ready(api, 2, undefined, registerReload(api, invoke));
    return;
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
          } catch (error) { return failure(api, replacement, 'after', error); }
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
  registerFinalGate(api, config.hooks.final ?? []);
  count += config.hooks.final?.length ?? 0;
  ready(api, count, degraded);
}
