/** Real TypeScript SDK/CLI adoption at a captured HTTP model boundary. */
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const require = createRequire(import.meta.url);
const sdk = await import(pathToFileURL(require.resolve('@anthropic-ai/claude-agent-sdk')));
const { z } = require('zod');
const { createQuery } = await import(pathToFileURL(process.env.AW_CLAUDE_SDK_BRIDGE));
const config = JSON.parse(readFileSync(process.env.AW_CLAUDE_CONFIG, 'utf8'));
const mode = process.env.CLAUDE_FINAL_CASE;
const bash = mode.startsWith('bash-');
const late = ['late', 'timeout', 'error'].some(suffix => mode.endsWith(suffix));
const enforced = mode.includes('-gate-');
const history = mode.endsWith('-history');
const toolCount = history ? 3 : 1;
const marker = 'AW_CLAUDE_FINAL_SECRET_MARKER';
const requests = [], trace = [], messages = [];
const save = (name, value) => writeFileSync(name, JSON.stringify(value, null, 2));
const fingerprints = () => Object.fromEntries(Object.entries(config.typescript.files)
  .map(([name, path]) => [name, createHash('sha256').update(readFileSync(path)).digest('hex')]));
const baseline = fingerprints();
const server = createServer(async (request, response) => {
  const buffers = [];
  for await (const chunk of request) buffers.push(chunk);
  const body = JSON.parse(Buffer.concat(buffers));
  requests.push(body);
  const done = requests.length === toolCount + 1;
  const result = JSON.stringify(body.messages).includes(marker) ? marker : 'PUBLIC_OK';
  const block = done ? { type: 'text', text: result } : { type: 'tool_use', id: `native_ts_call_${requests.length}`,
    name: bash ? 'Bash' : 'mcp__local__touch', input: bash ? {
      command: `printf ${history ? `SAFE_RESULT_${requests.length}` : 'PUBLIC_TOOL_RESULT'}; touch effect; test -z "\${BUN_OPTIONS:-}\${AW_CLAUDE_FINAL_CONFIG:-}" && touch preload-cleared`,
    } : { value: 'typescript' } };
  const message = { id: `msg_ts_${requests.length}`, type: 'message', role: 'assistant',
    model: 'claude-sonnet-4-6', content: [], stop_reason: null, stop_sequence: null,
    usage: { input_tokens: 10, output_tokens: 1 } };
  const events = [
    { type: 'message_start', message },
    { type: 'content_block_start', index: 0,
      content_block: done ? { type: 'text', text: '' } : { ...block, input: {} } },
    { type: 'content_block_delta', index: 0, delta: done
      ? { type: 'text_delta', text: block.text }
      : { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: done ? 'end_turn' : 'tool_use', stop_sequence: null },
      usage: { output_tokens: 1 } },
    { type: 'message_stop' },
  ];
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  response.end(events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
save('server.json', { port: server.address().port });
let query;
const started = performance.now();
try {
  query = await createQuery({ prompt: 'Use the fixture tool once and report its result.', options: {
    model: 'claude-sonnet-4-6', cwd: process.cwd(), settingSources: [], tools: bash ? ['Bash'] : [],
    allowedTools: [bash ? 'Bash' : 'mcp__local__touch'], maxTurns: toolCount + 1,
    mcpServers: { local: sdk.createSdkMcpServer({ name: 'local', tools: [sdk.tool('touch',
      'Write a fixture marker', { value: z.string() }, async ({ value }) => {
        assert(existsSync(`policy-${value}`));
        writeFileSync('effect', value);
        return { content: [{ type: 'text', text: 'PUBLIC_TOOL_RESULT' }] };
      })] }) },
    hooks: { PostToolUse: [{ hooks: [async (payload, call) => {
      // Wait for the real AW Post Provider to finish, even if SDK callbacks run concurrently.
      const deadline = Date.now() + 10000;
      for (;;) {
        const events = existsSync('policy-events.jsonl')
          ? readFileSync('policy-events.jsonl', 'utf8').trim().split('\n').map(JSON.parse) : [];
        const post = events.find(event => event.name === 'tool.after' && event.native?.tool_use_id === call);
        if (post) {
          // CLI completion follows Provider output. Poll the live service event ledger too.
          const status = JSON.parse(execFileSync(process.env.CLAUDE_TEST_AW,
            ['status', '--config', process.env.CLAUDE_TEST_CONFIG], { timeout: 5000 })).result;
          if (status.events === 0) { trace.push({ phase: 'aw_post_returned', call_id: call, input: post }); break; }
        }
        assert(Date.now() < deadline, 'AW Post did not finish');
        await new Promise(resolve => setTimeout(resolve, 10));
      }
      const output = late ? { hookSpecificOutput: { hookEventName: 'PostToolUse',
        updatedToolOutput: bash ? { ...payload.tool_response, stdout: marker } : marker } } : {};
      trace.push({ phase: 'application_post_returned', call_id: call, input: payload, output });
      return output;
    }] }] },
    env: { ANTHROPIC_BASE_URL: `http://127.0.0.1:${server.address().port}`, ANTHROPIC_API_KEY: 'local-fixture',
      CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1' },
  } });
  for await (const message of query) messages.push(message);
  const status = JSON.parse(execFileSync(process.env.CLAUDE_TEST_AW,
    ['status', '--config', process.env.CLAUDE_TEST_CONFIG], { timeout: 5000 })).result;
  assert.equal(status.events, 0);
  assert.equal(requests.length, toolCount + 1);
  assert.equal(trace.length, toolCount * 2);
  if (history) {
    const firstResults = new Map();
    for (let turn = 1; turn <= toolCount; turn++) {
      const blocks = requests[turn].messages.flatMap(message => Array.isArray(message.content) ? message.content : [])
        .filter(block => block.type === 'tool_result');
      assert.equal(blocks.length, turn);
      for (let index = 0; index < turn; index++) {
        assert.equal(blocks[index].tool_use_id, `native_ts_call_${index + 1}`);
        assert(blocks[index].content.startsWith(`SAFE_RESULT_${index + 1}`));
        assert.notEqual(blocks[index].is_error, true);
        const { cache_control, ...result } = blocks[index];
        if (!firstResults.has(result.tool_use_id)) firstResults.set(result.tool_use_id, result);
        assert.deepEqual(result, firstResults.get(result.tool_use_id));
        assert.equal(blocks[index].cache_control?.type, index === turn - 1 ? 'ephemeral' : undefined);
      }
    }
  }
  assert(!JSON.stringify(trace[0]).includes(marker));
  const leaked = late && !enforced;
  assert.equal(JSON.stringify(requests[1]).includes(marker), leaked);
  const result = messages.filter(message => message.type === 'result');
  assert.equal(result.length, 1);
  assert.equal(result[0].is_error, false);
  assert.equal(result[0].result, leaked ? marker : 'PUBLIC_OK');
  if (late && enforced) {
    const blocks = requests[1].messages.flatMap(message => Array.isArray(message.content) ? message.content : []);
    assert.equal(blocks.find(block => block.type === 'tool_result').content, 'AW withheld the final tool result');
  }
  assert(existsSync('effect'));
  if (bash) assert(existsSync('preload-cleared'));
  assert.deepEqual(fingerprints(), baseline);
  if (enforced) {
    const runtime = JSON.parse(readFileSync(new URL('./claude-final-ready.json',
      pathToFileURL(process.env.AW_CLAUDE_CONFIG)), 'utf8'));
    save('cli-process.json', { pid: runtime.pid });
    query.close();
    for (let attempt = 0; attempt < 100 && existsSync(`/proc/${runtime.pid}`); attempt++) {
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert(!existsSync(`/proc/${runtime.pid}`), 'SDK CLI process remains');
  }
  save('result.json', { case: mode, passed: true, sdk: config.typescript.sdk, cli: config.typescript.cli,
    platform: config.typescript.platform, sdk_form: 'typescript', model_requests: requests.length,
    model_received_marker: leaked, model_response: result[0].result, model_kind: 'deterministic_local_http',
    aw_post_before_application: true, events_closed_before_client_teardown: true,
    installed_files_unchanged: true, sha256: baseline, tool_side_effect: true,
    elapsed_ms: performance.now() - started, final_gate: enforced ? 'verified' : 'disabled',
    ...(history ? { safe_history_preserved: true, cache_breakpoint_moved: true, tool_calls: toolCount } : {}) });
} finally {
  query?.close();
  save('requests.json', requests);
  save('messages.json', messages);
  save('trace.json', trace);
  await new Promise(resolve => server.close(resolve));
}
