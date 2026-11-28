/** A real Qoder SDK query with only a synthetic local model configuration. */
import { pathToFileURL } from 'node:url';
import { readFileSync, closeSync } from 'node:fs';
import { createInterface } from 'node:readline';

const [sdk, endpoint, proxy] = process.argv.slice(2);
const tokenFd = process.env.SDK_TRANSPORT_TOKEN_FD;
const token = tokenFd ? readFileSync(Number(tokenFd), 'utf8').trim() : 'synthetic-fixture-key';
if (tokenFd) closeSync(Number(tokenFd));
const { query } = await import(pathToFileURL(sdk));
const input = createInterface({ input: process.stdin });
const lines = input[Symbol.asyncIterator]();
console.log(JSON.stringify({ ready: true }));
await lines.next();
let modelPolicyCalls = 0;
const resultKinds = [];
const assistantErrors = [];
const failureReasons = [];
const resultDiagnostics = [];
const run = query({
  prompt: 'SYNTHETIC_TRANSPORT_PROBE',
  options: {
    auth: { type: 'accessToken', accessToken: token },
    proxy, systemPrompt: 'Return FIXTURE_OK.',
    model: 'fixture/fixture', maxTurns: 1, settingSources: [], tools: [],
    env: { QODER_SDK_CUSTOM_BASE_URL_BYOK: '1' },
    settings: { providers: { fixture: {
      type: 'openai-compatible', baseUrl: endpoint + '/v1',
      apiKey: 'synthetic-fixture-key', model: 'fixture',
    } } },
    resolveModel: () => { modelPolicyCalls++; return { model: 'fixture/fixture' }; },
  },
});
try {
  let completed = false;
  for await (const message of run) {
    if (message.type === 'result') {
      completed = !message.is_error;
      const errors = JSON.stringify(message.errors ?? []).toLowerCase();
      resultDiagnostics.push({ error_count: message.errors?.length ?? 0, error_lengths: (message.errors ?? []).map(e => String(e).length), terminal_reason: /^[a-z_]{1,60}$/.test(message.terminal_reason ?? '') ? message.terminal_reason : null, error_code: /^[A-Z_0-9]{1,60}$/.test(String(message.error_code ?? '')) ? message.error_code : null });
      for (const marker of ['no_models_available', 'catalog not ready', 'custom provider', 'unauthorized', 'forbidden', 'model not found', 'authentication', 'initialization', 'not allowed', 'not available', 'cannot', 'failed', 'invalid', 'custom_model', 'network', 'byok', '400', '403', 'fetch', 'configuration', 'model policy', 'provider']) {
        if (errors.includes(marker)) failureReasons.push(marker.toLowerCase().replaceAll(' ', '_'));
      }
      if (/^[a-z_]+$/.test(message.subtype ?? '')) resultKinds.push(message.subtype);
    }
    if (message.type === 'assistant' && /^[a-z_]+$/.test(message.error ?? '')) assistantErrors.push(message.error);
  }
  console.log(JSON.stringify({ completed, model_policy_calls: modelPolicyCalls, result_kinds: resultKinds, assistant_errors: assistantErrors, failure_reasons: failureReasons, diagnostics: resultDiagnostics }));
  await lines.next();
} catch (error) {
  const authBlocked = error.code === 'QODER_WORKER_RUNTIME_ERROR' &&
    ['NetworkAttemptError', 'exchangePersonalToken', 'initFromAccessToken'].every(
      marker => (error.stderr ?? '').includes(marker));
  console.log(JSON.stringify({completed: false, error_type: error.name,
    blocked_reason: authBlocked ? 'offline_auth_initialization' : null,
    error_code: error.code ?? null, phase: 'sdk_initialization'}));
  await lines.next();
} finally {
  await run.close();
  input.close();
}
