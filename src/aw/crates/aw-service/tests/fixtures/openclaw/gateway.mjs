#!/usr/bin/env node
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

if (process.argv.includes('--version')) {
  console.log('OpenClaw 2026.9.6 (fixture)');
  process.exit(0);
}
const config = JSON.parse(readFileSync(process.env.OPENCLAW_CONFIG_PATH));
const root = process.env.FAKE_ROOT;
const directory = config.plugins.load.paths.at(-1);
const plugin = (await import(pathToFileURL(join(directory, 'index.mjs')))).default;
const callbacks = [];
plugin.register({ pluginConfig: config.plugins.entries['aw-native-hooks'].config,
  on: (name, handler) => callbacks.push({ name, handler }), logger: { warn: message => console.error(message) } });
for (const row of callbacks.filter(row => row.name === 'gateway_start')) row.handler();
process.env.AW_OPENCLAW_NATIVE_ENV_FIXTURE = 'loaded-by-native-host';
await new Promise(resolve => setTimeout(resolve, 100));
const event = { toolName: 'custom/tool', params: { command: process.env.FAKE_SCENARIO ?? 'ALLOW', literal: 'a b; $HOME' }, toolCallId: 'call', runId: 'run' };
const context = { sessionId: 'session', runId: 'run', toolCallId: 'call' };
let blocked = false;
for (const row of callbacks.filter(row => row.name === 'before_tool_call')) {
  if ((await row.handler(event, context))?.block) { blocked = true; break; }
}
if (!blocked) {
  writeFileSync(join(root, 'tool-ran'), 'executed');
  await Promise.all(callbacks.filter(row => row.name === 'after_tool_call').map(row => row.handler({ ...event, result: { content: ['native result'] } }, context)));
}
writeFileSync(join(root, 'generated.json'), JSON.stringify(config));
console.log(JSON.stringify({ blocked, credential: existsSync(join(process.env.OPENCLAW_STATE_DIR, 'credential-marker')),
  state: process.env.OPENCLAW_STATE_DIR, home: process.env.HOME, readOnly: process.env.OPENCLAW_CONFIG_READONLY }));
