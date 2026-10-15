// Optional pinned-runtime acceptance; regular CI uses openclaw.mjs fixtures.
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';
import { register } from '../../../adapters/openclaw/hooks.mjs';

const packageRoot = process.env.OPENCLAW_PACKAGE_DIR;
assert.ok(packageRoot, 'Set OPENCLAW_PACKAGE_DIR to the installed OpenClaw 2026.9.6 package');
assert.equal(JSON.parse(readFileSync(join(packageRoot, 'package.json'))).version, '2026.9.6');
const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-native-'));
process.env.OPENCLAW_HOME = root;
process.env.OPENCLAW_STATE_DIR = root;
process.env.OPENCLAW_CONFIG_PATH = join(root, 'openclaw.json');
const dist = join(packageRoot, 'dist');
const { sanitizeToolResult } = await import(pathToFileURL(join(dist, 'plugin-sdk/agent-harness-runtime.js')));
const plugin = { register: api => register(api, sanitizeToolResult) };
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
  const middlewares = [];
  plugin.register({
    pluginConfig: {
      binary: fileURLToPath(new URL('./fixtures/openclaw/client.mjs', import.meta.url)),
      binding: directory, ready: { path: join(directory, 'ready.json'), token: 'test' },
      hooks: {
        before: before.map(step => ({ step, onError: 'block', budgetMs: 1000 })),
        after: after.map(step => typeof step === 'string' ? ({ step, onError: 'report', budgetMs: 1000 }) : step),
      },
    },
    logger: { warn() {} },
    registerAgentToolResultMiddleware: handler => middlewares.push(handler),
    on: (hookName, handler, options) => typedHooks.push({
      hookName, handler, pluginId: 'aw-native-hooks', ...options,
    }),
  });
  return {
    middlewares,
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


test('official model-bound middleware runner adopts replacement text', { timeout: 10000 }, async t => {
  const source = readdirSync(dist).filter(name => /\.mjs$/.test(name))
    .map(name => [name, readFileSync(join(dist, name), 'utf8')])
    .find(([, text]) => text.includes('function createAgentToolResultMiddlewareRunner('));
  assert.ok(source);
  const alias = source[1].match(/createAgentToolResultMiddlewareRunner as (\w+)/)?.[1];
  assert.ok(alias);
  const createRunner = (await import(pathToFileURL(resolve(dist, source[0]))))[alias];
  const f = fixture(t, [], ['slow', 'first', { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  const runner = createRunner({ ...context, runtime: 'openclaw' }, f.middlewares);
  const result = await runner.applyToolResultMiddleware({ ...event, args: event.params,
    result: { content: [{ type: 'text', text: 'original' }] } });
  assert.equal(result.content.length, 1);
  assert.equal(result.content[0].text, 'compact');
  assert.deepEqual(f.records().filter(row => row.phase === 'start').map(row => row.step), ['replace']);
  await f.runner.runAfterToolCall({ ...event, result, durationMs: 57, error: undefined }, context);
  assert.deepEqual(f.records().filter(row => row.phase === 'start').map(row => row.step), ['replace', 'slow', 'first']);
  for (const row of f.records()) assert.equal(row.received.event.result.content[0].text, 'original');
  for (const row of f.records().filter(row => row.step !== 'replace')) assert.equal(row.received.event.durationMs, 57);
});

test('native completion keeps failures and redacts the original observation snapshot', { timeout: 10000 }, async t => {
  const f = fixture(t, [], ['first', { step: 'empty', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  const source = readdirSync(dist).filter(name => /^tool-result-middleware-.*\.mjs$/.test(name))[0];
  const { t: createRunner } = await import(pathToFileURL(join(dist, source)));
  const original = { content: [{ type: 'text', text: 'failure' }], details: { status: 'failed', authorization: 'Bearer private-review-secret' } };
  const result = await createRunner({ ...context, runtime: 'openclaw' }, f.middlewares)
    .applyToolResultMiddleware({ ...event, args: event.params, isError: true, result: original });
  await f.runner.runAfterToolCall({ ...event, result: sanitizeToolResult(result), error: 'native failure', durationMs: 81 }, context);
  const observed = f.records().find(row => row.step === 'first').received.event;
  assert.equal(observed.error, 'native failure');
  assert.equal(observed.durationMs, 81);
  assert.deepEqual(observed.result, sanitizeToolResult(original));
  assert.equal(JSON.stringify(observed).includes('private-review-secret'), false);
});

test('official plugin loader accepts the entrypoint, SDK import and middleware manifest contract', { timeout: 30000 }, async t => {
  const directory = mkdtempSync(join(root, 'loader-'));
  const entry = join(directory, 'plugin');
  mkdirSync(entry, { mode: 0o700 });
  for (const name of ['index.mjs', 'hooks.mjs', 'package.json', 'openclaw.plugin.json']) {
    copyFileSync(fileURLToPath(new URL(`../../../adapters/openclaw/${name}`, import.meta.url)), join(entry, name));
    chmodSync(join(entry, name), 0o600);
  }
  const loader = readdirSync(dist).find(name => /^loader-runtime-load-.*\.mjs$/.test(name));
  const { t: acquire } = await import(pathToFileURL(join(dist, loader)));
  const inspection = await acquire({ onlyPluginIds: ['aw-native-hooks'], config: { plugins: {
    enabled: true, allow: ['aw-native-hooks'], load: { paths: [entry] }, entries: { 'aw-native-hooks': {
      enabled: true, config: { binary: '/unused/aw', binding: '/unused/binding', ready: { path: join(directory, 'ready'), token: 'test' },
        hooks: { before: [{ step: 'scan', onError: 'block', budgetMs: 1000 }],
          after: [{ step: 'observe', onError: 'report', budgetMs: 1000 }, { step: 'compress', onError: 'report', budgetMs: 1000, replaceResult: true }] } },
    } },
  } } });
  try {
    const record = inspection.registry.plugins.find(plugin => plugin.id === 'aw-native-hooks');
    assert.equal(record?.status, 'loaded', JSON.stringify(inspection.registry.diagnostics));
    assert.equal(inspection.registry.agentToolResultMiddlewares.length, 1, JSON.stringify({ record, diagnostics: inspection.registry.diagnostics }));
    assert.ok(inspection.registry.typedHooks.some(hook => hook.hookName === 'before_tool_call'));
    assert.ok(inspection.registry.typedHooks.some(hook => hook.hookName === 'after_tool_call'));
    assert.ok(inspection.registry.typedHooks.some(hook => hook.hookName === 'session_end'));
    assert.ok(!inspection.registry.diagnostics.some(row => row.pluginId === 'aw-native-hooks' && row.level === 'error'));
  } finally { await inspection.release(); }
});
