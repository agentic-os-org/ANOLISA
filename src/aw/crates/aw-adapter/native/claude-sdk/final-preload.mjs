/** Install the verified Bun boundary before the embedded CLI evaluates its entry. */
import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { installFetchGate } from './shared/js-runtime/fetch-gate.mjs';
import { createToolGate } from './claude-final-gate.mjs';

const config = JSON.parse(readFileSync(process.env.AW_CLAUDE_FINAL_CONFIG, 'utf8'));
const root = dirname(fileURLToPath(import.meta.url));
if (process.env.BUN_OPTIONS !== `--preload=${fileURLToPath(import.meta.url)}` ||
    Bun.version !== config.final_profile.bun ||
    createHash('sha256').update(readFileSync('/proc/self/exe')).digest('hex') !==
      config.final_profile.sha256['_bundled/claude']) {
  throw Error('AW final gate runtime fingerprint mismatch');
}
delete process.env.BUN_OPTIONS;
delete process.env.AW_CLAUDE_FINAL_CONFIG;
const handlers = config.hooks.filter(hook => hook.event === 'security.violation');
if (!handlers.length || handlers.some(hook => hook.on_error !== 'block')) {
  throw Error('AW final gate requires fail-closed routes');
}

async function invoke(payload, signal) {
  const input = JSON.stringify(payload);
  if (Buffer.byteLength(input) > 1024 * 1024) return false;
  const transport = (hook, close = false) => new Promise((resolve, reject) => {
    let allowed = false;
    let inputError = false;
    const child = execFile(config.executable, ['hook', '--adapter', 'claude-sdk',
      '--binding', config.binding, '--event', 'security.violation',
      '--step', hook.step, '--on-error', 'block', ...(close ? ['--close-event', 'true'] : [])], {
      timeout: close ? 2500 : hook.budget_ms + 2500,
      maxBuffer: 1024 * 1024, signal: close ? undefined : signal, killSignal: 'SIGKILL',
    }, (error, stdout, stderr) => {
      if (stderr) process.stderr.write(stderr);
      allowed = !error && !inputError && !stdout.trim();
    });
    child.once('close', () => close && !allowed
      ? reject(Error('AW final event cleanup failed')) : resolve(allowed));
    child.stdin.on('error', () => { inputError = true; child.kill('SIGKILL'); });
    child.stdin.end(input);
  });
  let active;
  let completed = false;
  try {
    for (const hook of handlers) {
      if (signal?.aborted) return false;
      active = hook;
      if (!await transport(hook)) return false;
    }
    completed = true;
    return true;
  } finally {
    // Cleanup owns a fresh bounded transport even when the model request was cancelled.
    if (active && !completed) await transport(active, true);
  }
}

const gate = createToolGate(() => JSON.parse(readFileSync(`${root}/claude-final-records.json`, 'utf8')), invoke);
installFetchGate((url, init, input) => {
  const target = new URL(url);
  if (target.origin === config.origin && target.pathname === '/v1/messages') return gate;
  if (target.origin !== config.origin && init?.body == null &&
      !(input instanceof Request && input.body !== null)) return undefined;
  // Alternate routes and opaque bodies cannot carry an unchecked model request.
  return () => { throw Error('AW final gate model endpoint is unsupported'); };
});
writeFileSync(`${root}/claude-final-ready.json`, JSON.stringify({
  token: config.ready_token, profile: config.final_profile, pid: process.pid,
}), { mode: 0o600, flag: 'wx' });
