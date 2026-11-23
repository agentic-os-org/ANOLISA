/** Exercise the legacy dialect and foreground admission independently of hosted inference. */
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, copyFileSync, cpSync, writeFileSync, rmSync, readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { checkCallbacks, fixture, nativeReply } from '../shared/final-bridge-test.mjs';

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

test('Qoder final gate leaves native callbacks and ordinary policy intact in shadow', async t => {
  const original = process.env.AW_QODER_SDK_CONFIG;
  for (const mode of ['off', 'shadow', 'enforce']) {
    await t.test(mode, async t => {
      const root = mkdtempSync(join(tmpdir(), 'aw-qoder-final-callback-'));
      t.after(() => {
        if (original === undefined) delete process.env.AW_QODER_SDK_CONFIG;
        else process.env.AW_QODER_SDK_CONFIG = original;
        rmSync(root, { recursive: true, force: true });
      });
      const { file, entry, log } = fixture(root, 'qoder', mode);
      process.env.AW_QODER_SDK_CONFIG = file;
      // Each production final-launch module admits one Query.
      cpSync(import.meta.dirname, join(root, 'qoder-sdk'), { recursive: true });
      cpSync(join(import.meta.dirname, '../shared'), join(root, 'shared'), { recursive: true });
      copyFileSync(join(import.meta.dirname, '../claude-sdk/final-gate.mjs'), join(root, 'qoder-sdk/claude-final-gate.mjs'));
      const bridge = await import(pathToFileURL(join(root, 'qoder-sdk/bridge.mjs')));
      const { createQoderGate } = await import(pathToFileURL(join(root, 'qoder-sdk/final-gate.mjs')));
      const daemon = await bridge.startDaemon({});
      t.after(() => daemon.stop());
      const query = await bridge.createQuery({ prompt: 'text', options: {
        daemon, cwd: root, settingSources: [], tools: ['Bash'], persistSession: false,
        hooks: { PreToolUse: [{ hooks: [async () => nativeReply] }] },
      } });
      t.after(() => mode === 'off' ? query.return() : query.close());
      const sdk = await import(pathToFileURL(entry));
      await checkCallbacks(root, 'qoder', mode, sdk.options, log, createQoderGate);
    });
  }
});

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

test('legacy final preload preserves shadow bytes and withholds stale enforcing results', t => {
  const root = mkdtempSync(join(tmpdir(), 'aw-qoder-final-shadow-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(join(root, 'shared/js-runtime'), { recursive: true });
  for (const [source, dest] of [
    ['final-preload.mjs', 'preload.mjs'],
    ['final-gate.mjs', 'qoder-final-gate.mjs'],
    ['../claude-sdk/final-gate.mjs', 'claude-final-gate.mjs'],
    ['../shared/transport.mjs', 'shared/transport.mjs'],
    ['../shared/js-runtime/module-patch.mjs', 'shared/js-runtime/module-patch.mjs'],
    ['../shared/js-runtime/final-records.mjs', 'final-records.mjs'],
  ]) {
    copyFileSync(join(import.meta.dirname, source), join(root, dest));
  }
  const executable = join(root, 'aw');
  writeFileSync(executable, `#!/bin/sh
cat >/dev/null
printf '%s\\n' "$*" >> calls
for arg do
    [ "$arg" = "--close-event" ] && exit 0
done
exit 2
`, { mode: 0o700 });
  writeFileSync(join(root, 'qoder-final-records.json'), JSON.stringify({ call: native }));
  writeFileSync(join(root, 'config.json'), JSON.stringify({
    executable, binding: '/fixture', ready_token: 'fixture',
    hooks: [{ event: 'security.violation', step: 'shadow', on_error: 'report', budget_ms: 1000 }],
    final_profile: { worker_sha256: 'a'.repeat(64), patches: [{ anchor: 'old', replacement: 'new' }] },
  }));
  writeFileSync(join(root, 'run.mjs'), `
import assert from 'node:assert/strict';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { finalRecords } from './final-records.mjs';
const stale = process.env.MODE === 'stale';
if (stale) {
  const config = JSON.parse(readFileSync('config.json', 'utf8'));
  config.hooks[0].on_error = 'block';
  writeFileSync('config.json', JSON.stringify(config));
  const snapshots = finalRecords('.', 'qoder', true);
  snapshots.publish({ call: ${JSON.stringify(native)} });
  snapshots.close();
}
const preload = new URL('./preload.mjs', import.meta.url);
process.env.AW_QODER_FINAL_WORKER = '1';
process.env.AW_QODER_FINAL_CONFIG = 'config.json';
process.env.NODE_OPTIONS = '--import=' + preload.href;
await import(preload);
const body = ${JSON.stringify(request('SECRET') + '\n')};
const gate = globalThis[Symbol.for('aw.qoder.final-result')];
if (stale) {
  assert.equal(JSON.parse(await gate(body)).messages.at(-1).content, 'AW withheld the final tool result');
  assert.equal(existsSync('calls'), false);
} else {
assert.equal(await gate(body), body);
assert.equal(await gate('{"unsupported":true}'), '{"unsupported":true}');
const calls = readFileSync('calls', 'utf8').trim().split('\\n');
assert.equal(calls.length, 2);
assert(calls.every(call => call.includes('--step shadow --on-error report')));
assert(calls[1].includes('--close-event'));
}
`);
  for (const mode of ['shadow', 'stale']) {
    const result = spawnSync(process.execPath, [join(root, 'run.mjs')], {
      cwd: root, encoding: 'utf8', timeout: 5000, env: { ...process.env, MODE: mode },
    });
    assert.equal(result.status, 0, result.stderr);
    rmSync(join(root, 'calls'), { force: true });
  }
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
  t.after(() => launch.close());
  launch.prepare();
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
  assert.equal(JSON.parse(readFileSync(join(root, 'qoder-final-records.json'))).records.call.ambiguous, true);
  let callbackDone = false;
  const callback = launch.wait('PostToolUse', 'call').then(() => { callbackDone = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(callbackDone, false);
  launch.complete('PostToolUse', 'call'); await callback;
  assert.throws(() => finalLaunch(config, 'again', options));
});
