// Optional pinned-runtime acceptance; regular CI uses openclaw.mjs fixtures.
import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';
import plugin from '../../../adapters/openclaw/index.mjs';

const packageRoot = process.env.OPENCLAW_PACKAGE_DIR;
assert.ok(packageRoot, 'Set OPENCLAW_PACKAGE_DIR to the installed OpenClaw 2026.9.6 package');
assert.equal(JSON.parse(readFileSync(join(packageRoot, 'package.json'))).version, '2026.9.6');
const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-native-'));
process.env.OPENCLAW_HOME = root;
process.env.OPENCLAW_STATE_DIR = root;
process.env.OPENCLAW_CONFIG_PATH = join(root, 'openclaw.json');
const dist = join(packageRoot, 'dist');
const source = readdirSync(dist).filter(name => /^hooks-.*\.mjs$/.test(name))
  .map(name => [name, readFileSync(join(dist, name), 'utf8')])
  .find(([, text]) => text.includes('function createHookRunner('));
assert.ok(source, 'Pinned native hook runner must be present');
const alias = source[1].match(/createHookRunner as (\w+)/)?.[1];
assert.ok(alias, 'Pinned native hook runner must expose its export');
const createHookRunner = (await import(pathToFileURL(resolve(dist, source[0]))))[alias];
test.after(() => rmSync(root, { recursive: true }));

function fixture(t, before, after, existing = []) {
  const directory = mkdtempSync(join(root, 'case-'));
  const typedHooks = existing.slice();
  plugin.register({
    pluginConfig: {
      binary: fileURLToPath(new URL('./fixtures/openclaw/client.mjs', import.meta.url)),
      binding: directory, ready: { path: join(directory, 'ready.json'), token: 'test' },
      hooks: {
        before: before.map(step => ({ step, onError: 'block', budgetMs: 1000 })),
        after: after.map(step => ({ step, onError: 'report', budgetMs: 1000 })),
      },
    },
    logger: { warn() {} },
    on: (hookName, handler, options) => typedHooks.push({
      hookName, handler, pluginId: 'aw-native-hooks', ...options,
    }),
  });
  return {
    runner: createHookRunner({ typedHooks }, { catchErrors: false }),
    records: () => readFileSync(join(directory, 'calls.jsonl'), 'utf8').trim()
      .split('\n').map(row => JSON.parse(row)),
  };
}

const event = { toolName: 'custom/tool', params: { command: 'original' }, toolCallId: 'call', runId: 'run' };
const context = { sessionId: 'session', runId: 'run', toolCallId: 'call' };

test('official before runner keeps existing priority and immutable snapshots, then short-circuits block', { timeout: 10000 }, async t => {
  const existingCalls = [];
  const f = fixture(t, ['first', 'block', 'skipped'], [], [{
    hookName: 'before_tool_call', pluginId: 'existing', priority: 200,
    handler: original => {
      existingCalls.push(original.params.command);
      return { params: { command: 'existing rewrite' } };
    },
  }]);
  const result = await f.runner.runBeforeToolCall(event, context);
  assert.equal(result.block, true);
  assert.deepEqual(existingCalls, ['original']);
  const rows = f.records();
  assert.deepEqual(rows.filter(row => row.phase === 'start').map(row => row.step), ['first', 'block']);
  assert.equal(rows[0].received.event.params.command, 'original');
  assert.equal(rows[1].phase, 'end');
  assert.equal(rows[2].phase, 'start');
});

test('official after runner starts independent handlers concurrently and ignores their return values', { timeout: 10000 }, async t => {
  const existingCalls = [];
  const f = fixture(t, [], ['slow', 'first'], [{
    hookName: 'after_tool_call', pluginId: 'existing', priority: 200,
    handler: () => { existingCalls.push('after'); return { result: 'ignored' }; },
  }]);
  assert.equal(await f.runner.runAfterToolCall({ ...event, result: { content: [] } }, context), undefined);
  assert.deepEqual(existingCalls, ['after']);
  const rows = f.records();
  assert.ok(rows.findIndex(row => row.step === 'first' && row.phase === 'start')
    < rows.findIndex(row => row.step === 'slow' && row.phase === 'end'));
});
