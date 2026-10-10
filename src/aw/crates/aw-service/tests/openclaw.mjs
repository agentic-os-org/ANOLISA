import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import plugin from '../../../adapters/openclaw/index.mjs';

const binary = fileURLToPath(new URL('./fixtures/openclaw/client.mjs', import.meta.url));

function setup(t, before = [], after = []) {
  const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-plugin-'));
  t.after(() => rmSync(root, { recursive: true }));
  const callbacks = [];
  const warnings = [];
  const entry = item => typeof item === 'string'
    ? { step: item, onError: 'report', budgetMs: 1000 } : item;
  plugin.register({
    pluginConfig: {
      binary, binding: root, ready: { path: join(root, 'ready.json'), token: 'test-token' },
      hooks: { before: before.map(entry), after: after.map(entry) },
    },
    on: (name, callback, options) => callbacks.push({ name, callback, options }),
    logger: { warn: message => warnings.push(message) },
  });
  return { root, warnings, callbacks, rows: () => readFileSync(join(root, 'calls.jsonl'), 'utf8')
    .trim().split('\n').map(line => JSON.parse(line)) };
}

const event = { toolName: 'custom/tool', params: { words: ['a b', '$literal'], unicode: '你好' }, toolCallId: 'call-1', runId: 'run-1' };
const context = { sessionId: 'session-1', sessionKey: 'agent:main', toolCallId: 'call-1', runId: 'run-1' };

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
