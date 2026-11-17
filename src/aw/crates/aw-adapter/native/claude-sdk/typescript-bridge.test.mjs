import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';

test('TypeScript factory rejects unverified launch paths and gates first prompt on readiness', async () => {
  const root = mkdtempSync(join(tmpdir(), 'aw-ts-contract-'));
  const original = process.env.AW_CLAUDE_CONFIG;
  try {
    mkdirSync(join(root, 'shared'));
    copyFileSync(new URL('typescript-bridge.mjs', import.meta.url), join(root, 'bridge.mjs'));
    copyFileSync(new URL('../shared/transport.mjs', import.meta.url), join(root, 'shared/transport.mjs'));
    const entry = join(root, 'sdk.mjs');
    const ready = join(root, 'ready.json');
    writeFileSync(entry, `import assert from 'node:assert/strict';
import {existsSync} from 'node:fs';
export function query({prompt,options}) {
assert(!existsSync(${JSON.stringify(ready)}));
let initialized=false;
return {async initializationResult(){initialized=true;},close(){},
async *[Symbol.asyncIterator](){for await(const message of prompt){
assert(initialized);assert(existsSync(${JSON.stringify(ready)}));yield message;}}};}
`);
    const config = { executable: '/unused', binding: '/unused', ready_path: ready,
      ready_token: 'fixture', hooks: [], final_profile: null, typescript: { entry, binary: '/unused',
        files: { 'sdk/sdk.mjs': entry }, sha256: { 'sdk/sdk.mjs': createHash('sha256')
          .update(readFileSync(entry)).digest('hex') } } };
    process.env.AW_CLAUDE_CONFIG = join(root, 'config.json');
    writeFileSync(process.env.AW_CLAUDE_CONFIG, JSON.stringify(config));
    const { createQuery } = await import(pathToFileURL(join(root, 'bridge.mjs')));
    const options = { model: 'qwen3.8-flash', tools: ['Bash'], settingSources: [] };
    for (const change of [{ settingSources: undefined }, { tools: ['Agent'] }, { resume: 'old' },
      { model: 'auto' }, { model: 'bedrock/model' }, { spawnClaudeCodeProcess() {} },
      { pathToClaudeCodeExecutable: '/foreign' }, { mcpServers: { remote: { type: 'http' } } },
      { env: { BUN_OPTIONS: '--preload=foreign' } }, { env: { CLAUDE_CODE_USE_BEDROCK: '1' } },
      { hooks: { UserPromptSubmit: [] } }]) {
      await assert.rejects(createQuery({ prompt: 'text', options: { ...options, ...change } }));
    }
    const attempts = await Promise.allSettled([
      createQuery({ prompt: 'checked prompt', options }), createQuery({ prompt: 'concurrent', options }),
    ]);
    assert.equal(attempts[0].status, 'fulfilled');
    assert.equal(attempts[1].status, 'rejected');
    assert.match(attempts[1].reason.message, /one foreground Query/);
    const query = attempts[0].value;
    const messages = [];
    for await (const message of query) messages.push(message);
    assert.equal(messages[0].message.content, 'checked prompt');
    assert.equal(query.setMcpServers, undefined);
    assert.equal(query.setModel, undefined);
    await assert.rejects(createQuery({ prompt: 'second', options }), /one foreground Query/);
    query.close();
  } finally {
    if (original === undefined) delete process.env.AW_CLAUDE_CONFIG;
    else process.env.AW_CLAUDE_CONFIG = original;
    rmSync(root, { recursive: true, force: true });
  }
});


test('TypeScript matchers fail closed and interrupted callbacks close their events', async () => {
  const original = process.env.AW_CLAUDE_CONFIG;
  for (const mode of ['deny', 'error', 'unacknowledged', 'abort-before', 'abort-after', 'report', 'limit']) {
    const root = mkdtempSync(join(tmpdir(), 'aw-ts-callback-'));
    try {
      mkdirSync(join(root, 'shared'));
      copyFileSync(new URL('typescript-bridge.mjs', import.meta.url), join(root, 'bridge.mjs'));
      copyFileSync(new URL('../shared/transport.mjs', import.meta.url), join(root, 'shared/transport.mjs'));
      const entry = join(root, 'sdk.mjs'), executable = join(root, 'aw'), log = join(root, 'calls');
      writeFileSync(entry, `export let options; export function query(input) {options=input.options;
return {async initializationResult(){},close(){},async *[Symbol.asyncIterator](){}};}`);
      writeFileSync(executable, `#!/usr/bin/env node
import {appendFileSync} from 'node:fs';
let payload='';for await(const chunk of process.stdin)payload+=chunk;
appendFileSync(${JSON.stringify(log)},JSON.stringify({args:process.argv.slice(2),payload,pid:process.pid})+'\\n');
const close=process.argv.includes('--close-event');
if(close) process.exit(0);
if(${JSON.stringify(mode)}!=='unacknowledged') process.stdout.write(JSON.stringify({event_complete: ['deny','error'].includes(${JSON.stringify(mode)})}));
if(${JSON.stringify(mode)}.startsWith('abort')) await new Promise(resolve=>setTimeout(resolve,60000));
process.exit(${JSON.stringify(mode)}==='deny'?2:['error','report','unacknowledged'].includes(${JSON.stringify(mode)})?9:0);
`);
      chmodSync(executable, 0o700);
      const event = mode === 'abort-after' ? 'tool.after' : 'tool.before';
      const config = { executable, binding: 'fixture', ready_path: join(root, 'ready'), ready_token: 'fixture',
        hooks: ['first', 'second'].map(step => ({ event, step, budget_ms: 50,
          on_error: ['report', 'abort-after'].includes(mode) ? 'report' : 'block' })),
        typescript: { entry, binary: '/unused', files: {}, sha256: {} } };
      process.env.AW_CLAUDE_CONFIG = join(root, 'config');
      writeFileSync(process.env.AW_CLAUDE_CONFIG, JSON.stringify(config));
      const bridge = await import(pathToFileURL(join(root, 'bridge.mjs')));
      const query = await bridge.createQuery({ prompt: 'text', options: {
        model: 'fixture-model', tools: ['Bash'], settingSources: [],
      } });
      const sdk = await import(pathToFileURL(entry));
      const matcher = sdk.options.hooks[event === 'tool.before' ? 'PreToolUse' : 'PostToolUse'][0];
      assert.equal(matcher.onFailure, ['report', 'abort-after'].includes(mode) ? 'continue' : 'block');
      assert.equal(matcher.timeout, 9);
      const controller = new AbortController();
      const payload = { hook_event_name: event === 'tool.before' ? 'PreToolUse' : 'PostToolUse',
        tool_name: 'Bash', tool_use_id: 'call', tool_input: { command: 'true' } };
      if (mode === 'limit') {
        for (let id = 0; id < 3072; id++) await matcher.hooks[0](payload, String(id), { signal: controller.signal });
        await assert.rejects(matcher.hooks[0](payload, 'exhausted', { signal: controller.signal }), /identity limit/);
      } else {
        const pending = matcher.hooks[0](payload, 'call', { signal: controller.signal });
        if (mode.startsWith('abort')) {
          for (let attempt = 0; ; attempt++) {
            try { if (readFileSync(log, 'utf8')) break; } catch {}
            assert(attempt < 200, 'callback did not start');
            await new Promise(resolve => setTimeout(resolve, 10));
          }
          controller.abort(Error('cancelled'));
          await assert.rejects(pending, /cancelled/);
        } else {
          const result = await pending;
          assert.equal(result.hookSpecificOutput?.permissionDecision, mode === 'report' ? undefined : 'deny');
        }
        const calls = readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
        assert.equal(calls.length, ['deny', 'error'].includes(mode) ? 1 : 2);
        if (calls.length === 2) {
          assert.equal(calls[1].args.includes('--close-event'), mode !== 'report');
          assert.equal(calls[1].args.includes('second'), mode === 'report');
          assert.equal(calls[0].payload, calls[1].payload);
        }
        for (const call of calls) assert.throws(() => process.kill(call.pid, 0), { code: 'ESRCH' });
      }
      query.close();
    } finally {
      if (original === undefined) delete process.env.AW_CLAUDE_CONFIG;
      else process.env.AW_CLAUDE_CONFIG = original;
      rmSync(root, { recursive: true, force: true });
    }
  }
});
