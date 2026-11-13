/** Exercise the published resolver and transcript converter under the real preload. */
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { register } from '../../../../aw-adapter/native/openclaw/hooks.mjs';

const root = process.argv[2];
const version = JSON.parse(readFileSync(join(root, 'package.json'))).version;
const dist = join(root, 'dist');
const published = (pattern, declaration, exported) => {
  const name = readdirSync(dist).find(name => pattern.test(name) &&
    readFileSync(join(dist, name), 'utf8').includes(declaration));
  assert(name, declaration);
  const source = readFileSync(join(dist, name), 'utf8');
  const alias = source.match(new RegExp(`${exported} as (\\w+)`))[1];
  return { name, alias };
};
const resolver = published(/^(attempt\.model-diagnostic-events|session-boundary-prompt-cache-key)-/,
  'function resolveEmbeddedAgentStream(params)',
  version === '2026.9.7' ? 'selectEmbeddedAgentStream' : 'resolveEmbeddedAgentStream');
const select = (await import(pathToFileURL(join(dist, resolver.name))))[resolver.alias];
let sends = 0;
const custom = () => { sends++; return 'unchecked model send'; };
const runtime = { streamSimple: () => {}, registry: { getApiProvider: () => undefined } };
const params = { llmRuntime: runtime, model: { api: 'openai-completions', provider: 'fixture' }, sessionId: 'fixture' };
assert.throws(() => select({ ...params, providerStreamFn: custom }).streamFn(), /Provider stream override/);
assert.throws(() => select({ ...params, currentStreamFn: custom }).streamFn(), /verified boundary-aware stream/);
assert.equal(select(params).strategy, 'boundary-aware:openai-completions');
assert.equal(sends, 0);
const converter = published(/^session-/, 'function convertToLlm(messages)', 'convertToLlm');
const convertToLlm = (await import(pathToFileURL(join(dist, converter.name))))[converter.alias];
const history = [{ role: 'bashExecution', command: 'cat secret', output: 'AW_HISTORY_SECRET', timestamp: 1 }];
assert(JSON.stringify(convertToLlm(history)).includes('AW_HISTORY_SECRET'), 'native history red control');
register({ pluginConfig: { binary: '/unused', binding: '/unused', ready: { path: '/unused', token: 'fixture' },
  hooks: { before: [], after: [], final: [{ step: 'final', onError: 'block', budgetMs: 1000 }] } },
  registerAgentToolResultMiddleware() {}, on() {} }, value => value);
await assert.rejects(globalThis[Symbol.for('aw.openclaw.modelGate')](history, {
  ...params, convertToLlm,
}, undefined, rows => rows), /cannot verify a transcript output/);
console.log(JSON.stringify({ version, custom_stream_sends: sends, provider_stream_rejected: true,
  session_stream_rejected: true, native_history_red: true, unbound_history_rejected: true }));
