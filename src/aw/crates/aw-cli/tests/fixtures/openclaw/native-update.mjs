#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

if (process.argv.includes('--version')) {
  console.log('OpenClaw 2026.9.6 (fixture)');
  process.exit(0);
}
const overlay = JSON.parse(readFileSync(process.env.OPENCLAW_CONFIG_PATH));
const config = overlay.plugins.entries['aw-native-hooks'].config;
const binding = JSON.parse(readFileSync(config.binding));
const { register } = await import(pathToFileURL(join(overlay.plugins.load.paths.at(-1), 'hooks.mjs')));
const callbacks = new Map();
let middleware;
register({ pluginConfig: config, on: (name, callback) => callbacks.set(name, callback),
  registerAgentToolResultMiddleware: callback => { middleware = callback; },
  logger: { warn: message => process.stderr.write(message + '\n') },
}, result => structuredClone(result));
await callbacks.get('gateway_start')();
const repeated = spawnSync(config.binary, ['bridge', '--adapter', 'openclaw', '--binding', config.binding],
  { input: JSON.stringify({ phase: 'ready', key: '', pid: process.pid }),
    encoding: 'utf8', timeout: 5000, maxBuffer: 65536 });
assert.notEqual(repeated.status, 0);
assert.match(repeated.stderr, /bridge_already_ready/);
const pid = process.pid;
const update = () => spawnSync(config.binary, ['instance', 'update', '--instance', binding.binding.instance_id,
  '--config', process.env.AW_TEST_CONFIG, '--policy', process.env.AW_TEST_POLICY],
  { encoding: 'utf8', timeout: 20000, maxBuffer: 1024 * 1024 });
let context = { sessionId: 'session', runId: 'blocked-run' };
let event = { toolName: 'custom/tool', params: { command: '12345' }, toolCallId: 'blocked-call' };
assert.equal((await callbacks.get('before_agent_run')({}, context)).outcome, 'pass');
assert.notEqual(update().status, 0, 'busy update must fail');
assert.equal((await callbacks.get('before_tool_call')(event, context)).block, true);
await middleware({ ...event, args: event.params, result: 'blocked' }, context);
await callbacks.get('after_tool_call')(event, context);
await callbacks.get('agent_end')({ success: true }, context);
const accepted = update();
assert.equal(accepted.status, 0, accepted.stderr);
context = { ...context, runId: 'allowed-run' };
event = { ...event, toolCallId: 'allowed-call' };
assert.equal((await callbacks.get('before_agent_run')({}, context)).outcome, 'pass');
assert.notEqual((await callbacks.get('before_tool_call')(event, context))?.block, true);
writeFileSync(join(process.env.FAKE_ROOT, 'tool-ran'), 'executed');
await middleware({ ...event, args: event.params, result: 'native-result' }, context);
await callbacks.get('after_tool_call')(event, context);
await callbacks.get('agent_end')({ success: true }, context);
assert.equal(process.pid, pid);
writeFileSync(join(process.env.FAKE_ROOT, 'native-result.json'), JSON.stringify({ passed: true }));
