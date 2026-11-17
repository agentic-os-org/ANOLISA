/** Exercise the legacy dialect and foreground admission independently of hosted inference. */
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, copyFileSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const native = { session_id: 'session', tool_use_id: 'call', tool_name: 'Bash',
  cwd: '/work', tool_input: { command: 'echo safe' }, is_error: false };
const request = text => JSON.stringify({ session_id: 'session', session_type: 'qodercli',
  agent_id: 'agent_common', chat_task: 'FREE_INPUT', messages: [
    { role: 'system', content: 'system' }, { role: 'user', content: 'prompt' },
    { role: 'assistant', tool_calls: [{ id: 'call', type: 'function',
      function: { name: 'Bash', arguments: JSON.stringify(native.tool_input) } }] },
    { role: 'tool', tool_call_id: 'call', content: text },
  ] });
const result = value => JSON.parse(value).messages[3].content;

test('legacy final requests bind identities, replays and failure to checked text', async t => {
  const root = mkdtempSync(join(tmpdir(), 'aw-qoder-final-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  copyFileSync(join(import.meta.dirname, 'final-gate.mjs'), join(root, 'gate.mjs'));
  copyFileSync(join(import.meta.dirname, '../claude-sdk/final-gate.mjs'), join(root, 'claude-final-gate.mjs'));
  const { createQoderGate } = await import(pathToFileURL(join(root, 'gate.mjs')));
  let calls = 0;
  const identities = { call: native };
  const gate = createQoderGate(() => identities, async payload => {
    calls++;
    assert.equal(payload.hook_event_name, 'AWFinalToolResult');
    return !JSON.stringify(payload.tool_response).includes('SECRET');
  });
  const withheld = 'AW withheld the final tool result';
  assert.equal(result(await gate(request('SECRET'))), withheld);
  assert.equal(result(await gate(request('SECRET'))), withheld);
  assert.equal(result(await gate(request(withheld))), withheld);
  assert.equal(result(await gate(request('changed'))), withheld);
  assert.equal(calls, 1);
  const allowed = createQoderGate(() => identities, async () => true);
  assert.equal(result(await allowed(request('safe'))), 'safe');
  identities.call = { ...native, tool_input: { command: 'different' } };
  assert.equal(result(await allowed(request('safe'))), withheld);
  for (const records of [() => ({}), () => ({ call: { ...native, ambiguous: true } })])
    assert.equal(result(await createQoderGate(records, async () => true)(request('SECRET'))), withheld);
  identities.call = native;
  const annotated = JSON.parse(request('safe'));
  annotated.messages[1].cache_control = { type: 'ephemeral' };
  const cached = createQoderGate(() => identities, async () => true);
  assert.equal(result(await cached(JSON.stringify(annotated))), 'safe');
  assert.equal(result(await cached(request('safe'))), 'safe');
  annotated.messages[1].cache_control.secret = 'SECRET';
  await assert.rejects(cached(JSON.stringify(annotated)), /cache annotation/);
  assert.equal(result(await createQoderGate(() => identities, async () => { throw Error('provider down'); })(request('SECRET'))), withheld);
  for (const mutate of [r => { r.agent_id = 'other'; }, r => { r.messages[3].contents = ['SECRET']; },
    r => { r.messages[2].tool_calls[0].function.name = 'Task'; },
    r => { r.messages.push(r.messages[3]); },
    r => { r.messages.push({ role: 'user', content: 'background output' }); },
    r => { r.chat_context = { text: 'background output' }; }]) {
    const input = JSON.parse(request('SECRET')); mutate(input);
    await assert.rejects(gate(JSON.stringify(input)));
  }
  const failed = JSON.parse(request('tool failed'));
  failed.messages[3].is_error = true;
  const errors = [];
  const failureGate = createQoderGate(() => ({ call: { ...native, is_error: true } }), async payload => {
    errors.push(payload.tool_response.isError); return true;
  });
  assert.equal(result(await failureGate(JSON.stringify(failed))), 'tool failed');
  assert.deepEqual(errors, [true]);
  assert.equal(result(await createQoderGate(() => ({ call: { ...native, is_error: true } }),
    async () => false)(JSON.stringify(failed))), withheld);
  await assert.rejects(allowed(JSON.stringify(failed)), /error state/);
  failed.messages[3].is_error = 'true';
  await assert.rejects(failureGate(JSON.stringify(failed)), /result envelope/);
  delete failed.messages[3].is_error;
  await assert.rejects(failureGate(JSON.stringify(failed)), /error state/);
  const controller = new AbortController();
  controller.abort(Error('cancelled'));
  await assert.rejects(gate(request('SECRET'), controller.signal), /cancelled/);
});

test('foreground readiness precedes prompt release and unsupported options fail closed', async t => {
  const root = mkdtempSync(join(tmpdir(), 'aw-qoder-launch-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const { finalLaunch } = await import('./final-launch.mjs');
  const config = { ready_path: join(root, 'ready'), ready_token: 'nonce', final_profile: { target: 'fixture' } };
  const options = { daemon: {}, settingSources: [], persistSession: false, tools: ['Bash'], model: 'auto' };
  for (const extra of [{ daemon: undefined }, { resume: 'old' }, { tools: ['Task'] }, { settingSources: ['user'] },
    { resolveModel: () => ({ model: 'auto' }) }, { model: 'custom' },
    { env: { NODE_OPTIONS: '--import=other' } }, { env: { QODER_MODEL_TRANSPORT: 'http' } },
    { hooks: { UserPromptSubmit: [] } }])
    assert.throws(() => finalLaunch(config, 'prompt', { ...options, ...extra }));
  const launch = finalLaunch(config, 'prompt', options);
  const iterator = launch.input();
  let delivered = false;
  const pending = iterator.next().then(value => { delivered = true; return value; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(delivered, false);
  assert.throws(() => launch.verify());
  writeFileSync(join(root, 'qoder-final-ready.json'), JSON.stringify({ token: 'wrong', profile: config.final_profile }));
  assert.throws(() => launch.verify(), /readiness mismatch/);
  writeFileSync(join(root, 'qoder-final-ready.json'), JSON.stringify({ token: config.ready_token, profile: config.final_profile }));
  launch.verify();
  assert.equal(delivered, false);
  launch.release();
  assert.equal((await pending).value.message.content, 'prompt');
  assert.equal(launch.record({ ...native, hook_event_name: 'PreToolUse', tool_input: { run_in_background: true } }), false);
  launch.record({ ...native, hook_event_name: 'PostToolUse' });
  launch.record({ ...native, hook_event_name: 'PostToolUse' });
  assert.equal(JSON.parse(readFileSync(join(root, 'qoder-final-records.json'))).call.ambiguous, true);
  let callbackDone = false;
  const callback = launch.wait('PostToolUse', 'call').then(() => { callbackDone = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(callbackDone, false);
  launch.complete('PostToolUse', 'call'); await callback;
  assert.throws(() => finalLaunch(config, 'again', options));
});
