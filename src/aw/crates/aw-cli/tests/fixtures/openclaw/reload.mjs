import assert from 'node:assert/strict';
import { chmodSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import { register } from '../../../../aw-adapter/native/openclaw/hooks.mjs';

let nativeRunner;
if (process.env.OPENCLAW_PACKAGE_DIR) {
  const root = process.env.OPENCLAW_PACKAGE_DIR;
  assert.equal(JSON.parse(readFileSync(join(root, 'package.json'))).version, '2026.9.6');
  const dist = join(root, 'dist');
  const file = readdirSync(dist).find(name => /^hooks-.*\.mjs$/.test(name) &&
    readFileSync(join(dist, name), 'utf8').includes('function createHookRunner('));
  const text = readFileSync(join(dist, file), 'utf8');
  const alias = text.match(/createHookRunner as (\w+)/)[1];
  nativeRunner = (await import(pathToFileURL(join(dist, file))))[alias];
}

function fixture(t, binding, denial) {
  const root = mkdtempSync(join(tmpdir(), 'aw-openclaw-reload-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const binary = join(root, 'callback.mjs');
  writeFileSync(binary, `#!${process.execPath}\nimport {appendFileSync} from 'node:fs';\n` +
    `let input='';for await(const chunk of process.stdin)input+=chunk;const row={command:process.argv[2],...JSON.parse(input)};\n` +
    `appendFileSync(${JSON.stringify(join(root, 'calls.jsonl'))},JSON.stringify(row)+'\\n');process.stdout.write(JSON.stringify(row.command === 'hook' && process.argv.includes('tool.before') ? ${JSON.stringify(denial ?? {})} : {}));\n`);
  chmodSync(binary, 0o700);
  const hooks = new Map();
  let middleware;
  register({ pluginConfig: { binary, binding: binding ?? root, reloadable: true,
    hooks: { before: [], after: [] }, ready: { path: join(root, 'ready'), token: 'test' } },
    on: (name, callback) => hooks.set(name, callback),
    registerAgentToolResultMiddleware: callback => { middleware = callback; },
    logger: { warn() {} },
  }, result => structuredClone(result));
  const runner = nativeRunner?.({ typedHooks: [...hooks].map(([hookName, handler]) => ({
    pluginId: 'aw-native-hooks', hookName, handler,
  })), plugins: [] });
  const start = async (event, context) => runner
    ? (await runner.runBeforeAgentRun(event, context))?.decision
    : await hooks.get('before_agent_run')(event, context);
  const before = async (event, context) => runner
    ? await runner.runBeforeToolCall(event, context) : await hooks.get('before_tool_call')(event, context);
  const phases = () => readFileSync(join(root, 'calls.jsonl'), 'utf8').trim().split('\n')
    .map(line => JSON.parse(line)).filter(row => row.command === 'bridge').map(row => row.phase);
  return { hooks, middleware, start, before, phases, binding: binding ?? root,
    shared: globalThis[Symbol.for('aw.openclaw.reload.v1')].get(binding ?? root) };
}
const context = { sessionId: 'session', runId: 'run' };
const event = { toolName: 'exec', params: { command: 'true' }, toolCallId: 'call', runId: 'run' };

test('ready run admission and actual tool completion release one policy gate', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  assert.equal((await f.start({}, context)).outcome, 'pass');
  assert.notEqual((await f.before(event, context))?.block, true);
  await f.middleware({ ...event, args: event.params, result: 'result' }, context);
  await f.hooks.get('after_tool_call')(event, context);
  await f.hooks.get('agent_end')({ success: true }, context);
  assert.deepEqual(f.phases(), ['ready', 'request_start', 'request_end']);
});

test('missing request admission blocks tool execution', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  assert.equal((await f.before(event, context)).block, true);
  assert.deepEqual(f.phases(), ['ready']);
});

test('cancellation and missing tool completion retain busy admission', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  await f.start({}, context);
  await f.before(event, context);
  await f.hooks.get('agent_end')({ success: true }, context);
  const canceled = { ...context, runId: 'canceled' };
  await f.start({}, canceled);
  await f.hooks.get('agent_end')({ success: false }, canceled);
  assert.deepEqual(f.phases(), ['ready', 'request_start', 'request_start']);
});

test('duplicate run and tool identities fail closed', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  await f.start({}, context);
  assert.equal((await f.start({}, context)).outcome, 'block');
  assert.equal((await f.before(event, context)).block, true);
});


test('separate native registry views retain the admitted owner state', async t => {
  const gateway = fixture(t);
  await gateway.hooks.get('gateway_start')();
  const embedded = fixture(t, gateway.binding);
  assert.equal((await embedded.start({}, context)).outcome, 'pass');
  await embedded.before(event, context);
  await embedded.hooks.get('after_tool_call')({ ...event, error: 'blocked' }, context);
  await embedded.hooks.get('agent_end')({ success: true }, context);
  assert.deepEqual(embedded.phases(), ['request_start', 'request_end']);
});

test('run completion joins blocked tool after callbacks and ledger cleanup', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  await f.start({}, context);
  await f.before(event, context);
  const completion = f.hooks.get('after_tool_call')({ ...event, error: 'blocked' }, context);
  await f.hooks.get('agent_end')({ success: true }, context);
  await completion;
  assert.deepEqual(f.phases(), ['ready', 'request_start', 'request_end']);
});


test('completed identities cannot be reused and late callbacks cannot end a later run', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  assert.equal((await f.start({}, context)).outcome, 'pass');
  await f.hooks.get('agent_end')({ success: true }, context);
  assert.equal(f.shared.completed.size, 1);
  assert.equal(f.shared.runs.size, 0);
  assert.equal((await f.start({}, context)).outcome, 'block');
  const next = { ...context, runId: 'next-run' };
  assert.equal((await f.start({}, next)).outcome, 'pass');
  const before = [...f.phases()];
  await f.hooks.get('agent_end')({ success: true }, context);
  assert.equal((await f.before(event, context)).block, true);
  assert.deepEqual(f.phases(), before);
  assert.equal(f.shared.runs.size, 1);
  await f.hooks.get('agent_end')({ success: true }, next);
  assert.deepEqual(f.phases(), ['ready', 'request_start', 'request_end', 'request_start', 'request_end']);
});

test('bounded identity retention rejects new starts instead of evicting completed identities', async t => {
  const f = fixture(t);
  await f.hooks.get('gateway_start')();
  await f.start({}, context);
  await f.hooks.get('agent_end')({ success: true }, context);
  const retained = [...f.shared.completed][0];
  // Reach the production bound without spawning thousands of callback processes.
  for (let index = 1; index < 4095; index++) f.shared.completed.add(`completed-${index}`);
  const last = { ...context, runId: 'last-run' };
  assert.equal((await f.start({}, last)).outcome, 'pass');
  assert.equal((await f.start({}, { ...context, runId: 'overflow-active' })).outcome, 'block');
  await f.hooks.get('agent_end')({ success: true }, last);
  assert.equal(f.shared.completed.size, 4096);
  assert.equal((await f.start({}, { ...context, runId: 'overflow-completed' })).outcome, 'block');
  assert.equal((await f.start({}, context)).outcome, 'block');
  assert.ok(f.shared.completed.has(retained));
  assert.deepEqual(f.phases(), ['ready', 'request_start', 'request_end', 'request_start', 'request_end']);
});

test('native tool consumer retains Tokenless corrected arguments in its blocking result', async t => {
  const args = { command: 'rtk git status --short', cwd: '/work tree' };
  const prefix = '[tokenless:rewrite] Retry this tool with arguments: ';
  const denial = { block: true, blockReason: prefix + JSON.stringify(args) };
  const f = fixture(t, undefined, denial);
  await f.hooks.get('gateway_start')();
  await f.start({}, context);
  const result = await f.before(event, context);
  assert.equal(result.block, true);
  assert.equal(result.blockReason, denial.blockReason);
  assert.equal(result.params, undefined);
  assert.equal(result.requireApproval, undefined);
  assert.deepEqual(JSON.parse(result.blockReason.slice(prefix.length)), args);
  assert.deepEqual(event.params, { command: 'true' });
  await f.hooks.get('after_tool_call')({ ...event, error: result.blockReason }, context);
  await f.hooks.get('agent_end')({ success: true }, context);
  assert.deepEqual(f.phases(), ['ready', 'request_start', 'request_end']);
});
