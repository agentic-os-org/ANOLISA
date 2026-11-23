#!/usr/bin/env node
import assert from 'node:assert/strict';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

if (process.argv.includes('--version')) {
  console.log('OpenClaw 2026.9.6 (fixture)');
  process.exit(0);
}
const overlay = JSON.parse(readFileSync(process.env.OPENCLAW_CONFIG_PATH));
const config = overlay.plugins.entries['aw-native-hooks'].config;
const { register } = await import(pathToFileURL(join(overlay.plugins.load.paths.at(-1), 'hooks.mjs')));
const callbacks = new Map();
register({ pluginConfig: config, on: (name, callback) => callbacks.set(name, callback),
  registerAgentToolResultMiddleware() {}, logger: { warn() {} },
}, result => structuredClone(result));
await callbacks.get('gateway_start')();
const context = { sessionId: 'session', runId: 'retry-run' };
assert.equal((await callbacks.get('before_agent_run')({}, context)).outcome, 'pass');
const original = { toolName: 'exec', params: { command: 'original' }, toolCallId: 'original-call' };
const denied = await callbacks.get('before_tool_call')(original, context);
assert.equal(denied.block, true);
const prefix = '[tokenless:rewrite] Retry this tool with arguments: ';
assert.ok(denied.blockReason.startsWith(prefix), denied.blockReason);
const corrected = JSON.parse(denied.blockReason.slice(prefix.length));
assert.deepEqual(corrected, { command: 'rtk git status --short', cwd: '/work tree' });
assert.deepEqual(original.params, { command: 'original' });
if (!denied.block) writeFileSync(join(process.env.FAKE_ROOT, 'original-ran'), 'executed');
await callbacks.get('after_tool_call')({ ...original, error: denied.blockReason }, context);
const retry = { ...original, params: corrected, toolCallId: 'fresh-retry-call' };
const rechecked = await callbacks.get('before_tool_call')(retry, context);
assert.equal(rechecked.block, true);
assert.equal(rechecked.blockReason, 'Corrected call denied by security');
if (!rechecked.block) writeFileSync(join(process.env.FAKE_ROOT, 'retry-ran'), 'executed');
await callbacks.get('after_tool_call')({ ...retry, error: rechecked.blockReason }, context);
await callbacks.get('agent_end')({ success: true }, context);
assert.equal(existsSync(join(process.env.FAKE_ROOT, 'original-ran')), false);
assert.equal(existsSync(join(process.env.FAKE_ROOT, 'retry-ran')), false);
writeFileSync(join(process.env.FAKE_ROOT, 'retry-result.json'), JSON.stringify({
  blocked: denied.block, reason: denied.blockReason, corrected, rechecked: rechecked.block,
}));
