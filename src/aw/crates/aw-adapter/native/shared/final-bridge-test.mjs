/** Check the callbacks installed on a public SDK Query for all final gate modes. */
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, readFileSync, rmdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { createToolGate, WITHHELD } from '../claude-sdk/final-gate.mjs';
import { readFinalRecords } from './js-runtime/final-records.mjs';

export function fixture(root, adapter, mode) {
  const claude = adapter === 'claude';
  const entry = join(root, 'sdk.mjs'), executable = join(root, 'aw'), log = join(root, 'calls');
  const config = { sdk: entry, executable, binding: 'fixture', cwd: root,
    ready_path: join(root, 'ready'), ready_token: 'fixture',
    typescript: { entry, binary: '/unused', files: {}, sha256: {} },
    final_profile: mode === 'off' ? null : { target: 'fixture' },
    final_enforcing: mode === 'enforce',
    hooks: ['tool.before', 'tool.after'].map(event => ({
      event, step: event, on_error: 'report', budget_ms: 100, replace_result: false,
    })),
  };
  if (config.final_profile) {
    config.hooks.push({ event: 'security.violation', step: 'final', budget_ms: 100,
      replace_result: false, on_error: config.final_enforcing ? 'block' : 'report' });
  }
  const ready = JSON.stringify({ token: config.ready_token, profile: config.final_profile });
  const receipt = config.final_profile ?
    `writeFileSync(${JSON.stringify(join(root, `${adapter}-final-ready.json`))}, ${JSON.stringify(ready)});` : '';
  writeFileSync(entry, `import {writeFileSync} from 'node:fs';
export let options;
export async function startDaemon() { return { async stop() {} }; }
export function query(input) {
  options = input.options;
  return { async initializationResult() { ${receipt} },
    close() {}, async return() {}, async *[Symbol.asyncIterator]() {} };
}`);
  writeFileSync(executable, `#!/bin/sh
IFS= read -r payload || :
printf '%s\\n' "$payload" >> "\${0%/*}/calls"
printf '%s' '${claude ? '{"event_complete":true}' : '{}'}'
`, { mode: 0o700 });
  const file = join(root, 'config.json');
  writeFileSync(file, JSON.stringify(config));
  return { file, entry, log };
}

export const nativeReply = { systemMessage: 'native callback' };

export async function checkFinalResult(root, adapter, id, input, allowed, createGate = createToolGate) {
  const claude = adapter === 'claude';
  let calls = 0;
  const gate = createGate(() => readFinalRecords(root, adapter, true), () => { calls++; return true; });
  const body = claude ? {
    metadata: { user_id: JSON.stringify({ session_id: 'session' }) },
    messages: [
      { role: 'assistant', content: [{ type: 'tool_use', id, name: 'Bash', input }] },
      { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, content: 'protected output' }] },
    ],
  } : {
    session_id: 'session', session_type: 'qodercli', agent_id: 'agent_common', chat_task: 'FREE_INPUT',
    messages: [
      { role: 'system', content: 'system' }, { role: 'user', content: 'prompt' },
      { role: 'assistant', tool_calls: [{ id, type: 'function',
        function: { name: 'Bash', arguments: JSON.stringify(input) } }] },
      { role: 'tool', tool_call_id: id, content: 'protected output' },
    ],
  };
  const last = JSON.parse(await gate(JSON.stringify(body))).messages.at(-1);
  assert.equal(claude ? last.content[0].content : last.content, allowed ? 'protected output' : WITHHELD);
  assert.equal(calls, Number(allowed));
}

export async function checkCallbacks(root, adapter, mode, options, log, createGate) {
  const claude = adapter === 'claude', enforcing = mode === 'enforce';
  const { hooks } = options;
  const backgroundInput = { command: 'true', run_in_background: true };
  const foregroundInput = { command: 'true' };
  const policy = event => claude ? hooks[event][0] : hooks[event].at(-1);
  const invoke = (event, id, input = foregroundInput, matcher = policy(event)) => matcher.hooks[0]({
    hook_event_name: event, tool_name: 'Bash', tool_use_id: id, tool_input: input,
    session_id: 'session', cwd: root,
  }, id, {});
  if (claude) {
    assert.equal(policy('PreToolUse').onFailure, enforcing ? 'block' : 'continue');
    assert.equal(options.env.CLAUDE_CODE_DISABLE_BACKGROUND_TASKS, enforcing ? '1' : undefined);
  }
  assert.deepEqual(await invoke('PreToolUse', 'foreground'), {});
  const background = await invoke('PreToolUse', 'background', backgroundInput);
  const business = claude ? hooks.PreToolUse[1] : hooks.PreToolUse[0];
  if (enforcing) {
    assert.equal(background.hookSpecificOutput.permissionDecision, 'deny');
    await assert.rejects(invoke('PreToolUse', 'foreground', foregroundInput, business), /context injection/);
  } else {
    assert.deepEqual(background, {});
    assert.deepEqual(await invoke('PreToolUse', 'background', backgroundInput, business), nativeReply);
    assert.deepEqual(await invoke('PostToolUse', 'background', backgroundInput), {});
  }

  const temporary = join(root, `${adapter}-final-records.tmp`);
  const records = join(root, `${adapter}-final-records.json`);
  const read = () => {
    const data = JSON.parse(readFileSync(records, 'utf8'));
    return enforcing ? data.records : data;
  };
  let next = 0;
  if (enforcing) {
    assert.deepEqual(await invoke('PostToolUse', 'call-0'), {});
    await checkFinalResult(root, adapter, 'call-0', foregroundInput, true, createGate);
    mkdirSync(temporary);
    await assert.rejects(invoke('PostToolUse', 'call-0'), { code: 'EISDIR' });
    assert.equal(read()['call-0'].ambiguous, undefined);
    await checkFinalResult(root, adapter, 'call-0', foregroundInput, false, createGate);
    rmdirSync(temporary);
    assert.deepEqual(await invoke('PostToolUse', 'call-1'), {});
    assert.equal(read()['call-0'].ambiguous, true);
    await checkFinalResult(root, adapter, 'call-0', foregroundInput, false, createGate);
    await checkFinalResult(root, adapter, 'call-1', foregroundInput, true, createGate);
    next = 2;
  } else {
    mkdirSync(temporary);
    assert.deepEqual(await invoke('PostToolUse', 'write-failure'), {});
    rmdirSync(temporary);
  }
  for (let id = next; id < 1024; id++) {
    assert.deepEqual(await invoke('PostToolUse', `call-${id}`), {});
  }
  if (enforcing) {
    await assert.rejects(invoke('PostToolUse', 'overflow'), /identity limit/);
    assert.deepEqual(await invoke('PostToolUse', 'call-1023'), {});
    assert.equal(read()['call-1023'].ambiguous, true);
  } else {
    assert.deepEqual(await invoke('PostToolUse', 'overflow'), {});
  }
  const events = readFileSync(log, 'utf8').trim().split('\n')
    .map(line => { const event = JSON.parse(line); return `${event.hook_event_name}:${event.tool_use_id}`; });
  if (enforcing) assert.equal(events.filter(event => event === 'PostToolUse:call-0').length, 1);
  const visited = new Set(events);
  assert(visited.has('PreToolUse:foreground'));
  assert(visited.has('PostToolUse:call-1023'));
  for (const optional of ['PreToolUse:background', 'PostToolUse:background', 'PostToolUse:write-failure', 'PostToolUse:overflow']) {
    assert.equal(visited.has(optional), !enforcing, optional);
  }
  if (mode === 'off') {
    assert.equal(existsSync(records), false);
  } else {
    assert.deepEqual(Object.keys(read()),
      Array.from({ length: 1024 }, (_, id) => `call-${id}`));
  }
}
