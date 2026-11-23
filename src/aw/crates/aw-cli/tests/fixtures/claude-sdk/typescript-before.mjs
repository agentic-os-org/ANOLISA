/** Real SDK/CLI matcher failure adoption and AW multi-step event cleanup. */
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const mode = process.env.CLAUDE_FINAL_CASE;
const matcherFault = mode.includes('matcher');
const config = JSON.parse(readFileSync(process.env.AW_CLAUDE_CONFIG, 'utf8'));
const requests = [], messages = [];
const save = (name, value) => writeFileSync(name, JSON.stringify(value, null, 2));
const fingerprint = () => Object.fromEntries(Object.entries(config.typescript.files)
  .map(([name, file]) => [name, createHash('sha256').update(readFileSync(file)).digest('hex')]));
const original = fingerprint();
const status = () => JSON.parse(execFileSync(process.env.CLAUDE_TEST_AW,
  ['service', 'status', '--config', process.env.CLAUDE_TEST_CONFIG], { timeout: 5000 })).result;
if (matcherFault) {
  // Inject a callback fault between the real bridge and unmodified public SDK.
  // Unit coverage separately exercises the bridge's actual identity-limit throw.
  const facade = new URL('matcher-facade.mjs', pathToFileURL(`${process.cwd()}/`));
  writeFileSync(facade, `import {query as nativeQuery} from ${JSON.stringify(pathToFileURL(config.typescript.entry).href)};
import assert from 'node:assert/strict';
import {writeFileSync} from 'node:fs';
export function query(input) {
  const matcher=input.options.hooks.PreToolUse[0];
  assert.equal(matcher.onFailure,'block');
  assert(matcher.timeout>0 && matcher.timeout<=20);
  const callback=matcher.hooks[0]; let first=true;
  matcher.hooks[0]=async (...args)=>{
    if(!first)return callback(...args); first=false;
    writeFileSync('injected.json',JSON.stringify({onFailure:matcher.onFailure,timeout:matcher.timeout}));
    if(${JSON.stringify(mode)}.endsWith('error'))throw Error('injected callback failure');
    await new Promise((resolve,reject)=>{const signal=args[2].signal;
      if(signal.aborted)reject(signal.reason);
      else signal.addEventListener('abort',()=>reject(signal.reason),{once:true});});
  }; return nativeQuery(input);
}`);
  const injected = { ...config, typescript: { ...config.typescript, entry: facade.pathname } };
  save('matcher-config.json', injected);
  process.env.AW_CLAUDE_CONFIG = `${process.cwd()}/matcher-config.json`;
}
const { createQuery } = await import(pathToFileURL(process.env.AW_CLAUDE_SDK_BRIDGE));
const server = createServer(async (request, response) => {
  const buffers = [];
  for await (const chunk of request) buffers.push(chunk);
  requests.push(JSON.parse(Buffer.concat(buffers)));
  assert(requests.length <= 3);
  if (requests.length > 1) assert.equal(status().events, 0, 'event remains before next model request');
  const done = requests.length === 3;
  const block = done ? { type: 'text', text: 'COMPLETE' } : { type: 'tool_use',
    id: `before_${requests.length}`, name: 'Bash', input: {
      command: requests.length === 1 ? 'touch denied-effect' : 'touch safe-effect',
    } };
  const events = [
    { type: 'message_start', message: { id: `msg_${requests.length}`, type: 'message', role: 'assistant',
      model: 'claude-sonnet-4-6', content: [], stop_reason: null, stop_sequence: null,
      usage: { input_tokens: 10, output_tokens: 1 } } },
    { type: 'content_block_start', index: 0, content_block: done ? { type: 'text', text: '' } : { ...block, input: {} } },
    { type: 'content_block_delta', index: 0, delta: done ? { type: 'text_delta', text: block.text }
      : { type: 'input_json_delta', partial_json: JSON.stringify(block.input) } },
    { type: 'content_block_stop', index: 0 },
    { type: 'message_delta', delta: { stop_reason: done ? 'end_turn' : 'tool_use', stop_sequence: null },
      usage: { output_tokens: 1 } }, { type: 'message_stop' },
  ];
  response.writeHead(200, { 'content-type': 'text/event-stream' });
  response.end(events.map(event => `event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`).join(''));
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
save('server.json', { port: server.address().port });
let query;
try {
  query = await createQuery({ prompt: 'Execute the requested tools.', options: {
    model: 'claude-sonnet-4-6', tools: ['Bash'], allowedTools: ['Bash'], settingSources: [], maxTurns: 3,
    env: { ANTHROPIC_BASE_URL: `http://127.0.0.1:${server.address().port}`,
      ANTHROPIC_API_KEY: 'local-fixture', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1' },
  } });
  for await (const message of query) messages.push(message);
  assert.equal(requests.length, 3);
  assert(!existsSync('denied-effect'), 'failed before callback allowed a tool side effect');
  assert(existsSync('safe-effect'), 'subsequent tool did not execute');
  assert.equal(status().events, 0);
  assert.deepEqual(fingerprint(), original);
  const result = messages.find(message => message.type === 'result');
  assert.equal(result?.is_error, false);
  assert.equal(result.result, 'COMPLETE');
  const events = readFileSync('policy-events.jsonl', 'utf8').trim().split('\n').map(JSON.parse);
  const blocked = events.filter(event => event.name === 'tool.before' &&
    event.tool.input.command === 'touch denied-effect');
  assert.equal(blocked.length, matcherFault ? 0 : 1, 'later before step ran after denial');
  assert.equal(events.filter(event => event.name === 'tool.before' &&
    event.tool.input.command === 'touch safe-effect').length, 2);
  save('result.json', { case: mode, passed: true, sdk: config.typescript.sdk, cli: config.typescript.cli,
    platform: config.typescript.platform, model_requests: requests.length, tool_side_effect: false,
    later_tool: true, events_closed_before_client_teardown: true, installed_files_unchanged: true,
    model_kind: 'deterministic_local_http', model_response: result.result });
} finally {
  query?.close();
  save('requests.json', requests); save('messages.json', messages);
  await new Promise(resolve => server.close(resolve));
}
