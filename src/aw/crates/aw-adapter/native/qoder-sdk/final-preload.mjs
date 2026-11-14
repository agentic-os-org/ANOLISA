/** Verify both SDK Worker launch and the final legacy plaintext boundary. */
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { installVerifiedPatch, removeOwnPreload } from './shared/js-runtime/module-patch.mjs';
import { run } from './shared/transport.mjs';
import { createQoderGate } from './qoder-final-gate.mjs';

const configPath = process.env.AW_QODER_FINAL_CONFIG;
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const root = dirname(configPath), profile = config.final_profile;
removeOwnPreload(process.env.NODE_OPTIONS === `--import=${fileURLToPath(import.meta.url)}`
  ? fileURLToPath(import.meta.url) : import.meta.url);
const worker = process.env.AW_QODER_FINAL_WORKER === '1';
delete process.env.AW_QODER_FINAL_WORKER;
if (!worker) {
  installVerifiedPatch({ target: '/node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js',
    digest: profile.sdk_sha256, anchor: 'delete t.NODE_OPTIONS',
    replacement: `t.NODE_OPTIONS=${JSON.stringify('--import=' + import.meta.url)},t.AW_QODER_FINAL_WORKER="1",t.AW_QODER_FINAL_CONFIG=${JSON.stringify(configPath)}` });
} else {
  delete process.env.AW_QODER_FINAL_CONFIG;
  const handlers = config.hooks.filter(hook => hook.event === 'security.violation');
  if (!handlers.length || handlers.some(hook => hook.on_error !== 'block')) throw Error('AW requires fail-closed final routes');
  const invoke = async (payload, signal) => {
    const input = Buffer.from(JSON.stringify(payload));
    const transport = async (hook, close = false) => {
      const result = await run(config.executable, ['hook', '--adapter', 'qoder-sdk',
        '--binding', config.binding, '--event', 'security.violation', '--step', hook.step,
        '--on-error', 'block', ...(close ? ['--close-event', 'true'] : [])], input,
      close ? 2500 : hook.budget_ms + 3000, close ? undefined : signal);
      if (close && result.code !== 0) throw Error('AW Qoder final cleanup failed');
      return result.code === 0 && !result.stdout.toString().trim();
    };
    let active, completed = false;
    try {
      for (const hook of handlers) {
        if (signal?.aborted) return false;
        active = hook;
        if (!await transport(hook)) return false;
      }
      completed = true;
      return true;
    } finally {
      if (active && !completed) await transport(active, true);
    }
  };
  const gate = createQoderGate(
    () => JSON.parse(readFileSync(`${root}/qoder-final-records.json`, 'utf8')), invoke);
  globalThis[Symbol.for('aw.qoder.final-result')] = async (body, signal) => {
    try { return await gate(body, signal); }
    catch (error) { process.stderr.write(`AW Qoder final gate rejected request: ${error.message}\n`); throw error; }
  };
  installVerifiedPatch({ target: '/node_modules/@qoder-ai/qoder-agent-sdk/dist/_worker/qoder-worker-runtime.obf.mjs',
    digest: profile.worker_sha256, patches: profile.patches,
    onPatched: () => writeFileSync(`${root}/qoder-final-ready.json`, JSON.stringify({
      token: config.ready_token, profile, pid: process.pid,
    }), { mode: 0o600, flag: 'wx' }) });
}
