import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { register } from '../../aw-adapter/native/openclaw/hooks.mjs';
const plugin = { register: api => register(api, result => structuredClone(result)) };

const binary = fileURLToPath(new URL('./fixtures/openclaw/client.mjs', import.meta.url));

function setup(t, before = [], after = [], middleware = true, final = []) {
  globalThis[Symbol.for('aw.openclaw.streamPatched')] = true;
  const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-plugin-'));
  t.after(() => rmSync(root, { recursive: true }));
  const callbacks = [];
  const middlewares = [];
  const warnings = [];
  const entry = item => typeof item === 'string'
    ? { step: item, onError: 'report', budgetMs: 1000 } : item;
  plugin.register({
    pluginConfig: {
      binary, binding: root, ready: { path: join(root, 'ready.json'), token: 'test-token' },
      hooks: { before: before.map(entry), after: after.map(entry), final: final.map(entry) },
    },
    registerAgentToolResultMiddleware: middleware ? callback => middlewares.push(callback) : undefined,
    on: (name, callback, options) => callbacks.push({ name, callback, options }),
    logger: { warn: message => warnings.push(message) },
  });
  t.after(() => callbacks.find(row => row.name === 'gateway_stop')?.callback());
  return { root, warnings, callbacks, middlewares, rows: () => readFileSync(join(root, 'calls.jsonl'), 'utf8')
    .trim().split('\n').map(line => JSON.parse(line)) };
}

test('final gate binds repeated call IDs to sessions and rejects ambiguous generations',
  { timeout: 10000 }, async t => {
    const f = setup(t, [], [], true, ['first']);
    const gate = globalThis[Symbol.for('aw.openclaw.modelGate')];
    const payloadGate = globalThis[Symbol.for('aw.openclaw.payloadGate')];
    globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
    globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
    t.after(() => {
      delete globalThis[Symbol.for('aw.openclaw.finalPatched')];
      delete globalThis[Symbol.for('aw.openclaw.providerPatched')];
    });
    const result = { role: 'toolResult', toolName: 'exec', toolCallId: 'samecall',
      content: [{ type: 'text', text: 'safe' }] };
    const registerResult = sessionId => f.middlewares[0](
      { toolName: 'exec', toolCallId: 'same-call', args: {} },
      { sessionId, runId: sessionId });
    const project = async sessionId => {
      await gate([result], { sessionId, model: { api: 'openai-completions' },
        convertToLlm: messages => messages }, undefined, messages => messages);
      return payloadGate({ messages: [{ role: 'tool', tool_call_id: 'samecall', content: 'safe' }] },
        sessionId, { messages: [{ role: 'tool', tool_call_id: 'samecall', content: 'safe' }] },
        [result], 'openai-completions');
    };
    registerResult('session-a');
    registerResult('session-b');
    assert.equal((await project('session-a')).messages[0].content, 'safe');
    assert.equal((await project('session-b')).messages[0].content, 'safe');
    assert.equal(f.rows().filter(row => row.event === 'security.violation' && row.phase === 'start').length, 2);

    registerResult('session-a');
    registerResult('session-a');
    assert.match((await project('session-a')).messages[0].content, /AW withheld/);
    assert.equal(f.rows().filter(row => row.event === 'security.violation' && row.phase === 'start').length, 2);
    registerResult('session-a');
    assert.equal((await project('session-a')).messages[0].content, 'safe');
    assert.equal(f.rows().filter(row => row.event === 'security.violation' && row.phase === 'start').length, 3);
  });

test('final gate rejects context transforms that add model-visible content', async t => {
  const f = setup(t, [], [], true, ['first']);
  const gate = globalThis[Symbol.for('aw.openclaw.modelGate')];
  globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
  globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
  t.after(() => {
    delete globalThis[Symbol.for('aw.openclaw.finalPatched')];
    delete globalThis[Symbol.for('aw.openclaw.providerPatched')];
  });
  const source = [{ role: 'user', content: [{ type: 'text', text: 'ordinary prompt' }] },
    { role: 'toolResult', toolName: 'exec', toolCallId: 'call-1',
      content: [{ type: 'text', text: 'safe' }] }];
  const injected = { role: 'user', content: [{ type: 'text', text: 'copied secret from tool' }] };
  const config = { sessionId: 'session-1', model: { api: 'openai-completions' },
    convertToLlm: messages => messages };
  await assert.rejects(gate(source, { ...config, transformContext: messages => [...messages, injected] },
    undefined, messages => messages), /context transform changed/);
  await assert.rejects(gate(source, { ...config, convertToLlm: messages =>
    messages.some(message => message.role === 'toolResult') ? [...messages, injected] : messages },
    undefined, messages => messages), /result conversion changed/);
  await assert.rejects(gate(structuredClone(source), { ...config, transformContext: messages => {
    messages[0].content[0].text = 'secret copied in place';
    return messages;
  } }, undefined, messages => messages), /context transform changed/);
  let retained;
  const delivered = await gate(source, { ...config, convertToLlm: messages => {
    if (messages.some(message => message.role === 'toolResult')) retained = messages;
    else retained[0].content[0].text = 'delayed converter mutation';
    return messages;
  } }, undefined, messages => messages);
  assert.equal(delivered[0].content[0].text, 'ordinary prompt');
  assert.equal(existsSync(join(f.root, 'calls.jsonl')), false);
});

test('provider gate checks the exact post-hook payload and rejects non-tool rewrites',
  { timeout: 10000 }, async t => {
    const f = setup(t, [], [], true, ['block']);
    const gate = globalThis[Symbol.for('aw.openclaw.payloadGate')];
    globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
    globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
    t.after(() => {
      delete globalThis[Symbol.for('aw.openclaw.finalPatched')];
      delete globalThis[Symbol.for('aw.openclaw.providerPatched')];
    });
    f.middlewares[0]({ toolName: 'exec', toolCallId: 'call-1', args: {} }, context);
    const original = [{ role: 'user', content: 'prompt' },
      { role: 'tool', tool_call_id: 'call-1', content: 'safe' }];
    const changed = original.map(message => message.role === 'tool'
      ? { ...message, content: 'AW_FINAL_GATE_SECRET_MARKER' } : message);
    const source = [{ role: 'toolResult', toolCallId: 'call-1', toolName: 'exec',
      content: [{ type: 'text', text: 'safe' }] }];
    const reply = await gate({ messages: changed }, context.sessionId, { messages: original },
      source, 'openai-completions');
    assert.equal(reply.messages[1].content, 'AW withheld the final tool result');
    assert.equal(f.rows()[0].received.event.result.content[0].text, 'AW_FINAL_GATE_SECRET_MARKER');
    await assert.rejects(gate({ messages: [{ ...original[0], content: 'copied secret' }, original[1]] },
      context.sessionId, { messages: original }, source, 'openai-completions'), /changed a model message identity/);
    await assert.rejects(gate({ messages: [original[0], { ...original[1], name: 'copied secret' }] },
      context.sessionId, { messages: original }, source, 'openai-completions'), /changed a model message identity/);
  });

test('normalized call-ID collisions withhold without selecting a native call', async t => {
  const f = setup(t, [], [], true, ['first']);
  globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
  globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
  t.after(() => {
    delete globalThis[Symbol.for('aw.openclaw.finalPatched')];
    delete globalThis[Symbol.for('aw.openclaw.providerPatched')];
  });
  for (const toolCallId of ['call-1', 'call_1']) {
    f.middlewares[0]({ toolName: 'exec', toolCallId, args: {} }, context);
  }
  const tool = { role: 'tool', tool_call_id: 'call1', content: 'secret' };
  const source = { role: 'toolResult', toolCallId: 'call1', toolName: 'exec',
    content: [{ type: 'text', text: 'secret' }] };
  const reply = await globalThis[Symbol.for('aw.openclaw.payloadGate')](
    { messages: [tool] }, context.sessionId, { messages: [tool] }, [source], 'openai-completions');
  assert.match(reply.messages[0].content, /AW withheld/);
  assert.equal(existsSync(join(f.root, 'calls.jsonl')), false);
});

const event = { toolName: 'custom/tool', params: { words: ['a b', '$literal'], unicode: '你好' }, toolCallId: 'call-1', runId: 'run-1' };
const context = { sessionId: 'session-1', sessionKey: 'agent:main', toolCallId: 'call-1', runId: 'run-1' };

test('final checks own their payload and native argument snapshots across awaits', async t => {
  const f = setup(t, [], [], true, ['slow', 'first']);
  globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
  globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
  const native = { toolName: 'exec', toolCallId: 'call-1', args: { command: 'safe' } };
  const nativeContext = { ...context };
  f.middlewares[0](native, nativeContext);
  native.args.command = 'changed before check';
  const messages = [{ role: 'user', content: 'prompt' },
    { role: 'tool', tool_call_id: 'call-1', content: 'safe' }];
  const source = [{ role: 'toolResult', toolCallId: 'call-1', toolName: 'exec' }];
  const checking = globalThis[Symbol.for('aw.openclaw.payloadGate')](
    { messages }, context.sessionId, { messages: structuredClone(messages) }, source, 'openai-completions');
  messages[0].content = 'unchecked prompt';
  messages[1].content = 'unchecked secret';
  nativeContext.runId = 'changed during check';
  const delivered = await checking;
  assert.deepEqual(delivered.messages.map(row => row.content), ['prompt', 'safe']);
  for (const row of f.rows()) {
    assert.equal(row.received.event.params.command, 'safe');
    assert.equal(row.received.context.runId, context.runId);
  }
});

for (const invalidation of ['session_end', 'gateway_stop', 'new_generation']) {
  test(`final check cannot publish after ${invalidation}`, async t => {
    const f = setup(t, [], [], true, ['slow']);
    globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
    globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
    const native = { toolName: 'exec', toolCallId: 'call-1', args: {} };
    f.middlewares[0](native, context);
    const messages = [{ role: 'tool', tool_call_id: 'call-1', content: 'safe' }];
    const source = [{ role: 'toolResult', toolCallId: 'call-1', toolName: 'exec' }];
    const gate = globalThis[Symbol.for('aw.openclaw.payloadGate')];
    const checking = gate({ messages }, context.sessionId, { messages }, source, 'openai-completions');
    if (invalidation === 'new_generation') f.middlewares[0](native, context);
    else f.callbacks.find(row => row.name === invalidation).callback({}, context);
    assert.match((await checking).messages[0].content, /AW withheld/);
    assert.match((await gate({ messages }, context.sessionId, { messages }, source,
      'openai-completions')).messages[0].content, /AW withheld/);
  });
}

test('final gate checks encoded bytes and rejects rewrites outside tool content', async t => {
  const f = setup(t, [], [], true, ['block']);
  globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
  globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
  f.middlewares[0]({ toolName: 'exec', toolCallId: 'call-1', args: {} }, context);
  const params = { messages: [{ role: 'tool', tool_call_id: 'call-1', content: 'safe' }],
    tools: [{ type: 'function', function: { name: 'exec', description: 'ordinary' } }] };
  const source = [{ role: 'toolResult', toolCallId: 'call-1', toolName: 'exec' }];
  const gate = globalThis[Symbol.for('aw.openclaw.payloadGate')];
  const delivered = await gate(params, context.sessionId, params, source,
    'openai-completions', async payload => {
      payload.messages[0].content = 'encoder secret';
      return { body: new TextEncoder().encode(JSON.stringify(payload)) };
    });
  assert.match(delivered.messages[0].content, /AW withheld/);
  assert.equal(f.rows()[0].received.event.result.content[0].text, 'encoder secret');
  const changed = structuredClone(params);
  changed.tools[0].function.description = 'copied secret';
  await assert.rejects(gate(changed, context.sessionId, params, source,
    'openai-completions'), /changed non-message payload/);
});

test('one native registration per step preserves order without overriding native priority', { timeout: 10000 }, async t => {
  const f = setup(t, ['first', 'second'], ['third']);
  assert.deepEqual(f.callbacks.map(row => [row.name, row.options]), [
    ['before_tool_call', undefined], ['before_tool_call', undefined],
    ['after_tool_call', undefined], ['gateway_start', undefined],
  ]);
  for (const row of f.callbacks.slice(0, 3)) await row.callback(event, context);
  assert.deepEqual(f.rows().filter(row => row.phase === 'start').map(row => row.step), ['first', 'second', 'third']);
  assert.deepEqual(f.rows()[0].received, { hook: 'before_tool_call', event, context });
  assert.deepEqual(f.rows()[4].received, { hook: 'after_tool_call', event, context });
});

test('native object and empty output are passed back to OpenClaw', { timeout: 10000 }, async t => {
  const f = setup(t, ['block', 'empty', 'first']);
  assert.deepEqual(await f.callbacks[0].callback(event, context), { block: true, blockReason: 'fixture policy' });
  assert.equal(await f.callbacks[1].callback(event, context), undefined);
  assert.deepEqual(await f.callbacks[2].callback(event, context), {
    params: { command: 'native output', unicode: '你好' }, fixture: 'first',
  });
});

test('execution and JSON failures obey the configured before failure action', { timeout: 10000 }, async t => {
  const entries = ['error', 'signal', 'invalid', 'array'].map(step => ({ step, onError: 'block', budgetMs: 1000 }));
  const f = setup(t, [...entries, 'error'], ['error']);
  for (const row of f.callbacks.slice(0, entries.length)) {
    assert.equal((await row.callback(event, context)).block, true);
  }
  assert.equal(await f.callbacks[entries.length].callback(event, context), undefined);
  assert.equal(await f.callbacks[entries.length + 1].callback(event, context), undefined);
  assert.equal(f.warnings.length, 6);
});

test('serialization failures cannot bypass before on_error=block', async t => {
  const f = setup(t, [{ step: 'first', onError: 'block', budgetMs: 1000 }]);
  const cyclic = {}; cyclic.cycle = cyclic;
  assert.equal((await f.callbacks[0].callback(cyclic, context)).block, true);
  assert.equal(existsSync(join(f.root, 'calls.jsonl')), false);
});

test('parallel native dispatch does not become a serial bridge queue', { timeout: 10000 }, async t => {
  const f = setup(t, [], ['slow', 'first']);
  await Promise.all(f.callbacks.slice(0, 2).map(row => row.callback(event, context)));
  const rows = f.rows();
  assert.ok(rows.findIndex(row => row.step === 'first' && row.phase === 'start')
    < rows.findIndex(row => row.step === 'slow' && row.phase === 'end'));
});

test('a timed-out callback is reaped before reporting a fail-closed result', { timeout: 6000 }, async t => {
  const f = setup(t, [{ step: 'timeout', onError: 'block', budgetMs: 1 }]);
  assert.equal((await f.callbacks[0].callback(event, context)).block, true);
  const pid = f.rows()[0].pid;
  assert.equal(existsSync(`/proc/${pid}`), false);
});

test('readiness is attested only by gateway_start after hook registration', async t => {
  const f = setup(t, ['first'], ['second']);
  const path = join(f.root, 'ready.json');
  assert.equal(existsSync(path), false);
  f.callbacks.find(row => row.name === 'gateway_start').callback();
  assert.deepEqual(JSON.parse(readFileSync(path)), {
    version: 1, adapter: 'openclaw', token: 'test-token', pid: process.pid, hooks: 2,
  });
});


test('replacement registers the model-bound middleware instead of an observer', { timeout: 10000 }, async t => {
  const f = setup(t, [], [{ step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  assert.equal(f.middlewares.length, 1);
  assert.deepEqual(f.callbacks.map(row => row.name), ['gateway_start']);
  const result = await f.middlewares[0]({ ...event, args: event.params, result: { content: [{ type: 'text', text: 'original' }] } }, context);
  assert.deepEqual(result, { result: { content: [{ type: 'text', text: 'compact' }], metadata: { kept: true } } });
  assert.deepEqual(f.rows()[0].received.event.params, event.params);
  f.callbacks.find(row => row.name === 'gateway_start').callback();
  const receipt = JSON.parse(readFileSync(join(f.root, 'ready.json')));
  assert.equal(receipt.hooks, 1);
  assert.equal(Object.hasOwn(receipt, 'degraded'), false);
});

test('native completion fields and the original result survive compression and observer failures', { timeout: 10000 }, async t => {
  const f = setup(t, [], ['slow', 'first', 'error',
    { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  assert.equal(f.middlewares.length, 1);
  assert.deepEqual(f.callbacks.map(row => row.name), ['after_tool_call', 'session_end', 'gateway_stop', 'gateway_start']);
  const native = { ...event, args: event.params, result: { content: [{ type: 'text', text: 'original' }] } };
  const snapshot = structuredClone(native);
  const result = await f.middlewares[0](native, context);
  assert.equal(result.result.content[0].text, 'compact');
  assert.deepEqual(native, snapshot);
  const completed = { ...event, result: result.result, durationMs: 37, error: 'native failure metadata', extra: 'preserved' };
  await f.callbacks.find(row => row.name === 'after_tool_call').callback(completed, context);
  const rows = f.rows();
  assert.deepEqual(rows.filter(row => row.phase === 'start').map(row => row.step), ['replace', 'slow', 'first', 'error']);
  assert.ok(rows.findIndex(row => row.step === 'slow' && row.phase === 'end')
    < rows.findIndex(row => row.step === 'first' && row.phase === 'start'));
  for (const row of rows) {
    assert.deepEqual(row.received.event.result, native.result);
    if (row.step !== 'replace') {
      assert.equal(row.received.event.error, completed.error);
      assert.equal(row.received.event.durationMs, 37);
      assert.equal(row.received.event.extra, 'preserved');
      assert.equal(row.completion.phase, 'observation');
    }
  }
  assert.equal(f.warnings.length, 1);
});

test('missing replacement middleware reports degraded readiness and retains working hooks', { timeout: 10000 }, async t => {
  const f = setup(t, [{ step: 'block', onError: 'block', budgetMs: 1000 }],
    ['observe', { step: 'replace', replaceResult: true }], false);
  assert.equal(f.middlewares.length, 0);
  assert.deepEqual(await f.callbacks.find(row => row.name === 'before_tool_call').callback(event, context),
    { block: true, blockReason: 'fixture policy' });
  await f.callbacks.find(row => row.name === 'after_tool_call').callback(event, context);
  assert.deepEqual(f.rows().filter(row => row.phase === 'start').map(row => row.step), ['block', 'observe']);
  f.callbacks.find(row => row.name === 'gateway_start').callback();
  assert.deepEqual(JSON.parse(readFileSync(join(f.root, 'ready.json'))), {
    version: 1, adapter: 'openclaw', token: 'test-token', pid: process.pid, hooks: 3,
    degraded: 'result_middleware_unavailable',
  });
  assert.equal(f.warnings.length, 1);
});

// The service validates content; this bridge must preserve field presence.
test('present falsy replacements differ from absent replacement and empty stdout', { timeout: 10000 }, async t => {
  for (const [step, result] of [['replace-empty', ''], ['replace-null', null], ['replace-false', false], ['replace-zero', 0], ['first', undefined], ['empty', undefined]]) {
    const f = setup(t, [], [{ step, onError: 'report', budgetMs: 1000, replaceResult: true }]);
    const reply = await f.middlewares[0]({ ...event, args: event.params, result: 'original' }, context);
    assert.deepEqual(reply, result === undefined ? undefined : { result }, step);
    assert.deepEqual(f.warnings, []);
  }
});

test('completion snapshots are isolated by run and released at completion', { timeout: 10000 }, async t => {
  const f = setup(t, [], ['first', { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  for (const runId of ['run-a', 'run-b']) {
    await f.middlewares[0]({ ...event, runId, args: event.params, result: { content: [{ type: 'text', text: runId }] } }, { ...context, runId });
  }
  for (const runId of ['run-b', 'run-a']) {
    await f.callbacks.find(row => row.name === 'after_tool_call').callback({ ...event, runId,
      result: { content: [{ type: 'text', text: 'compact' }] }, durationMs: 1 }, { ...context, runId });
  }
  assert.deepEqual(f.rows().filter(row => row.step === 'first' && row.phase === 'start')
    .map(row => row.received.event.result.content[0].text), ['run-b', 'run-a']);
});

test('bounded snapshot retention skips compression and session teardown releases capacity', { timeout: 10000 }, async t => {
  const f = setup(t, [], ['first', { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  const native = { ...event, args: event.params, result: { content: [{ type: 'text', text: 'original' }] } };
  for (let index = 0; index < 32; index++) {
    await f.middlewares[0]({ ...native, toolCallId: `call-${index}` }, context);
  }
  assert.equal(await f.middlewares[0]({ ...native, toolCallId: 'over-limit' }, context), undefined);
  assert.equal(f.warnings.length, 1);
  f.callbacks.find(row => row.name === 'session_end').callback({ sessionId: context.sessionId }, context);
  assert.equal((await f.middlewares[0]({ ...native, toolCallId: 'after-teardown' }, context)).result.content[0].text, 'compact');
  assert.equal(f.rows().filter(row => row.phase === 'start').length, 33);
  f.callbacks.find(row => row.name === 'gateway_stop').callback();
});

test('unbound transcript outputs fail before native conversion', async t => {
  setup(t, [], [], true, ['first']);
  const gate = globalThis[Symbol.for('aw.openclaw.modelGate')];
  for (const role of ['bashExecution', 'custom', 'branchSummary', 'compactionSummary']) {
    await assert.rejects(gate([{ role, output: 'history-secret' }], {
      convertToLlm: () => assert.fail('unbound output reached conversion'),
    }, undefined, rows => rows), /cannot verify a transcript output/);
  }
});

test('request and session cancellation reap the active check without dispatching later steps',
  { timeout: 10000 }, async t => {
    for (const cancellation of ['signal', 'session_end', 'gateway_stop']) {
      const f = setup(t, [], [], true, ['timeout', 'must-not-run']);
      globalThis[Symbol.for('aw.openclaw.finalPatched')] = true;
      globalThis[Symbol.for('aw.openclaw.providerPatched')] = true;
      f.middlewares[0]({ toolName: 'exec', toolCallId: 'call-1', args: {} }, context);
      const messages = [{ role: 'tool', tool_call_id: 'call-1', content: 'secret' }];
      const source = [{ role: 'toolResult', toolCallId: 'call-1', toolName: 'exec' }];
      const controller = new AbortController();
      const checking = globalThis[Symbol.for('aw.openclaw.payloadGate')](
        { messages }, context.sessionId, { messages }, source, 'openai-completions', undefined, controller.signal);
      for (let attempt = 0; attempt < 200 && !existsSync(join(f.root, 'calls.jsonl')); attempt++) {
        await new Promise(resolve => setTimeout(resolve, 5));
      }
      const started = f.rows();
      assert.equal(started.length, 1);
      if (cancellation === 'signal') controller.abort();
      else f.callbacks.find(row => row.name === cancellation).callback({ sessionId: context.sessionId }, {});
      assert.match((await checking).messages[0].content, /AW withheld/);
      assert.equal(f.rows().length, 1, 'another step or the cancelled callback completed');
      assert.equal(existsSync(`/proc/${started[0].pid}`), false, 'cancelled callback remains alive');
      assert.equal(readFileSync(join(f.root, 'cleanup.jsonl'), 'utf8').trim().split('\n').length, 1);
    }
  });
