import assert from 'node:assert/strict';
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const [root, native, executable] = process.argv.slice(2);
const mode = readFileSync(`${root}/mode`, 'utf8');
const stopStep = readFileSync(`${root}/step`, 'utf8');
mkdirSync(`${root}/shared`);
copyFileSync(`${native}/claude-sdk/typescript-bridge.mjs`, `${root}/bridge.mjs`);
copyFileSync(`${native}/shared/transport.mjs`, `${root}/shared/transport.mjs`);
writeFileSync(`${root}/sdk.mjs`, `export let options;export function query(input){options=input.options;
return {async initializationResult(){},close(){},async *[Symbol.asyncIterator](){}};}`);
process.env.AW_CLAUDE_CONFIG = `${root}/config.json`;
writeFileSync(process.env.AW_CLAUDE_CONFIG, JSON.stringify({ executable,
  binding: `${root}/binding.json`, ready_path: `${root}/ready`, ready_token: 'fixture',
  hooks: ['first', 'last'].map(step => ({ event: 'tool.before', step, budget_ms: 1000,
    on_error: mode === 'report' ? 'report' : 'block' })),
  typescript: { entry: `${root}/sdk.mjs`, binary: '/unused', files: {}, sha256: {} },
}));
const bridge = await import(pathToFileURL(`${root}/bridge.mjs`));
const query = await bridge.createQuery({ prompt: 'fixture', options: {
  model: 'fixture-model', tools: ['Bash'], settingSources: [],
} });
try {
  const sdk = await import(pathToFileURL(`${root}/sdk.mjs`));
  const matcher = sdk.options.hooks.PreToolUse[0];
  const controller = new AbortController();
  const payload = { session_id: 'receipt-session', cwd: root, hook_event_name: 'PreToolUse',
    tool_name: 'Bash', tool_use_id: 'receipt-call', tool_input: { mode, stop_step: stopStep, command: 'true' } };
  const pending = matcher.hooks[0](payload, payload.tool_use_id, { signal: controller.signal });
  // Register rejection handling before aborting the transport.
  const result = pending.then(value => ({ value }), error => ({ error }));
  if (mode === 'abort') {
    for (let i = 0; !existsSync(`${root}/abort-ready`); i++) {
      assert(i < 400, 'InvokeStep did not reach the real service boundary');
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    controller.abort(Error('fixture cancellation'));
  }
  const outcome = await result;
  if (mode === 'cleanup-error') assert.match(outcome.error?.message ?? '', /event cleanup failed/);
  else if (mode === 'abort') assert.match(outcome.error?.message ?? '', /fixture cancellation/);
  else {
    assert.ifError(outcome.error);
    assert.equal(outcome.value.hookSpecificOutput?.permissionDecision, mode === 'report' ? undefined : 'deny');
  }
} finally { query.close(); }
