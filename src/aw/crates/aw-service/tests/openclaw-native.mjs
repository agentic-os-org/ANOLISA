// Optional pinned-runtime acceptance; regular CI uses openclaw.mjs fixtures.
import assert from 'node:assert/strict';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';

const packageRoot = process.env.OPENCLAW_PACKAGE_DIR;
assert.ok(packageRoot, 'Set OPENCLAW_PACKAGE_DIR to the installed OpenClaw 2026.9.6 package');
assert.equal(JSON.parse(readFileSync(join(packageRoot, 'package.json'))).version, '2026.9.6');
const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-native-'));
process.env.OPENCLAW_HOME = root;
process.env.OPENCLAW_STATE_DIR = root;
process.env.OPENCLAW_CONFIG_PATH = join(root, 'openclaw.json');
const dist = join(packageRoot, 'dist');
const { sanitizeToolResult } = await import(pathToFileURL(join(dist, 'plugin-sdk/agent-harness-runtime.js')));
const source = readdirSync(dist).filter(name => /^hooks-.*\.mjs$/.test(name))
  .map(name => [name, readFileSync(join(dist, name), 'utf8')])
  .find(([, text]) => text.includes('function createHookRunner('));
assert.ok(source, 'Pinned native hook runner must be present');
const alias = source[1].match(/createHookRunner as (\w+)/)?.[1];
assert.ok(alias, 'Pinned native hook runner must expose its export');
const createHookRunner = (await import(pathToFileURL(resolve(dist, source[0]))))[alias];
test.after(() => rmSync(root, { recursive: true }));

async function fixture(t, before, after, existing = []) {
  const directory = mkdtempSync(join(root, 'case-'));
  const entry = join(directory, 'plugin');
  mkdirSync(entry, { mode: 0o700 });
  for (const name of ['index.mjs', 'hooks.mjs', 'package.json', 'openclaw.plugin.json']) {
    copyFileSync(fileURLToPath(new URL(`../../aw-adapter/native/openclaw/${name}`, import.meta.url)), join(entry, name));
    chmodSync(join(entry, name), 0o600);
  }
  const loader = readdirSync(dist).find(name => /^loader-runtime-load-.*\.mjs$/.test(name));
  assert.ok(loader, 'Pinned official plugin loader must be present');
  const { t: acquire } = await import(pathToFileURL(join(dist, loader)));
  const inspection = await acquire({ onlyPluginIds: ['aw-native-hooks'], config: { plugins: {
    enabled: true, allow: ['aw-native-hooks'], load: { paths: [entry] }, entries: { 'aw-native-hooks': {
      enabled: true, config: {
        binary: fileURLToPath(new URL('./fixtures/openclaw/client.mjs', import.meta.url)),
        binding: directory, ready: { path: join(directory, 'ready.json'), token: 'test' },
        hooks: {
          before: before.map(step => ({ step, onError: 'block', budgetMs: 1000 })),
          after: after.map(step => typeof step === 'string' ? ({ step, onError: 'report', budgetMs: 1000 }) : step),
        },
      },
    } },
  } } });
  t.after(() => inspection.release());
  const { registry } = inspection;
  const record = registry.plugins.find(plugin => plugin.id === 'aw-native-hooks');
  assert.equal(record?.status, 'loaded', JSON.stringify(registry.diagnostics));
  assert.ok(!registry.diagnostics.some(row => row.pluginId === 'aw-native-hooks' && row.level === 'error'));
  return {
    registry,
    middlewares: registry.agentToolResultMiddlewares.map(row => row.handler),
    runner: createHookRunner({ typedHooks: [...existing, ...registry.typedHooks] }, { catchErrors: false }),
    records: () => readFileSync(join(directory, 'calls.jsonl'), 'utf8').trim()
      .split('\n').map(row => JSON.parse(row)),
  };
}

const event = { toolName: 'custom/tool', params: { command: 'original' }, toolCallId: 'call', runId: 'run' };
const context = { sessionId: 'session', runId: 'run', toolCallId: 'call' };

test('official before runner keeps existing priority and immutable snapshots, then short-circuits block', { timeout: 10000 }, async t => {
  const existingCalls = [];
  const f = await fixture(t, ['first', 'block', 'skipped'], [], [{
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
  const f = await fixture(t, [], ['slow', 'first'], [{
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
  const f = await fixture(t, [], ['slow', 'first', { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
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
  const f = await fixture(t, [], ['first', { step: 'empty', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  const source = readdirSync(dist).filter(name => /^tool-result-middleware-.*\.mjs$/.test(name))[0];
  const { t: createRunner } = await import(pathToFileURL(join(dist, source)));
  const original = { content: [{ type: 'text', text: 'failure' }], details: { status: 'failed', authorization: 'Bearer private-review-secret' } };
  const result = await createRunner({ ...context, runtime: 'openclaw' }, f.middlewares)
    .applyToolResultMiddleware({ ...event, args: event.params, isError: true, result: original });
  assert.deepEqual(result, original, 'Failed native results must remain unchanged');
  await f.runner.runAfterToolCall({ ...event, result: sanitizeToolResult(result), error: 'native failure', durationMs: 81 }, context);
  const observed = f.records().find(row => row.step === 'first').received.event;
  assert.equal(observed.error, 'native failure');
  assert.equal(observed.durationMs, 81);
  assert.deepEqual(observed.result, sanitizeToolResult(original));
  assert.equal(JSON.stringify(observed).includes('private-review-secret'), false);
});

test('official before runner allows the tool when policy does not block', { timeout: 10000 }, async t => {
  const f = await fixture(t, ['first'], []);
  const result = await f.runner.runBeforeToolCall(event, context);
  assert.notEqual(result?.block, true);
  assert.deepEqual(f.records().filter(row => row.phase === 'start').map(row => row.step), ['first']);
});

test('official plugin loader accepts the entrypoint, SDK import and middleware manifest contract', { timeout: 30000 }, async t => {
  const f = await fixture(t, ['first'], ['first', { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  const registrations = f.registry.agentToolResultMiddlewares;
  assert.equal(registrations.length, 1);
  assert.deepEqual(registrations[0].runtimes, ['openclaw']);
  for (const name of ['before_tool_call', 'after_tool_call', 'session_end']) {
    assert.ok(f.registry.typedHooks.some(hook => hook.hookName === name), name);
  }
  const original = { content: [{ type: 'text', text: 'original' }] };
  const input = { ...event, args: event.params, result: original };
  assert.equal(await f.middlewares[0](input, { ...context, runtime: 'codex' }), undefined);
  assert.equal((await f.middlewares[0](input, { ...context, runtime: 'openclaw' })).result.content[0].text, 'compact');
  assert.deepEqual(f.records().filter(row => row.phase === 'start').map(row => row.step), ['replace']);
});

test('native policy aliases correlate replacement with the original completion snapshot', { timeout: 30000 }, async t => {
  const policy = readdirSync(dist).find(name => /^tool-policy-[^.]+\.mjs$/.test(name)
    && readFileSync(join(dist, name), 'utf8').includes('export {')
    && readFileSync(join(dist, name), 'utf8').includes('normalizeToolPolicyName,'));
  assert.ok(policy, 'Pinned native tool policy must expose normalization');
  const { normalizeToolPolicyName } = await import(pathToFileURL(join(dist, policy)));
  const middleware = readdirSync(dist).find(name => /^tool-result-middleware-.*\.mjs$/.test(name));
  const { t: createRunner } = await import(pathToFileURL(join(dist, middleware)));
  const f = await fixture(t, [], ['first', { step: 'replace', onError: 'report', budgetMs: 1000, replaceResult: true }]);
  const runner = createRunner({ ...context, runtime: 'openclaw' }, f.middlewares);
  for (const name of ['bash', 'exec', 'apply-patch', 'apply_patch', 'cron', 'automations', 'CuStOm/ToOl']) {
    // Expected completion names come from OpenClaw, independently of AW's mapping.
    const toolName = normalizeToolPolicyName(name);
    const toolCallId = `alias-${name}`;
    const original = { content: [{ type: 'text', text: `original-${name}` }] };
    const result = await runner.applyToolResultMiddleware({ ...event, toolName: name, toolCallId, args: event.params, result: original });
    assert.equal(result.content[0].text, 'compact', name);
    await f.runner.runAfterToolCall({ ...event, toolName, toolCallId, result, durationMs: 37 }, context);
    const observed = f.records().filter(row => row.step === 'first' && row.phase === 'start').at(-1);
    assert.equal(observed.received.event.toolName, toolName);
    assert.equal(observed.received.event.toolCallId, toolCallId);
    assert.deepEqual(observed.received.event.result, original, name);
    assert.equal(observed.received.event.durationMs, 37);
    assert.equal(observed.completion.phase, 'observation');
  }
});

test('native middleware distinguishes falsy replacement fields from absent output', { timeout: 30000 }, async t => {
  for (const [step, replacement] of [['replace-empty', ''], ['replace-null', null], ['replace-false', false], ['replace-zero', 0], ['first', undefined], ['empty', undefined]]) {
    const f = await fixture(t, [], [{ step, onError: 'report', budgetMs: 1000, replaceResult: true }]);
    const result = await f.middlewares[0]({ ...event, args: event.params, result: { content: [{ type: 'text', text: 'original' }] } },
      { ...context, runtime: 'openclaw' });
    assert.deepEqual(result, replacement === undefined ? undefined : { result: replacement }, step);
  }
});
