/** Keep the legacy final gate bound to one foreground Query and native identities. */
import { readFileSync, writeFileSync, renameSync } from 'node:fs';
import { dirname } from 'node:path';
import { isDeepStrictEqual } from 'node:util';

let created = false;
let active;

export function finalLaunch(config, prompt, options) {
  if (!config.final_profile) return undefined;
  const allowed = ['daemon', 'cwd', 'settingSources', 'persistSession', 'permissionMode',
    'tools', 'allowedTools', 'maxTurns', 'hooks', 'model', 'env'];
  if (created || !options.daemon || typeof prompt !== 'string' || JSON.stringify(options.settingSources) !== '[]' ||
      JSON.stringify(options.tools) !== '["Bash"]' || options.persistSession !== false ||
      Object.keys(options).some(key => !allowed.includes(key))) {
    throw Error('AW Qoder final gate requires one fresh foreground Bash Query');
  }
  if (options.model !== undefined && options.model !== 'auto') throw Error('AW Qoder final gate requires the native auto model');
  for (const env of [process.env, options.env ?? {}]) {
    if ((env.QODER_MODEL_TRANSPORT && env.QODER_MODEL_TRANSPORT !== 'legacy') || env.NODE_OPTIONS) {
      throw Error('AW Qoder final gate does not cover transport or loader overrides');
    }
  }
  if (Object.keys(options.hooks ?? {}).some(event => !['PreToolUse', 'PostToolUse', 'PostToolUseFailure'].includes(event))) {
    throw Error('AW Qoder final gate covers only native tool Hooks');
  }
  created = true;
  const root = dirname(config.ready_path), records = Object.create(null), completions = new Map();
  const save = () => {
    writeFileSync(`${root}/qoder-final-records.tmp`, JSON.stringify(records), { mode: 0o600 });
    renameSync(`${root}/qoder-final-records.tmp`, `${root}/qoder-final-records.json`);
  };
  save();
  let release, failed = false;
  const ready = new Promise(resolve => { release = resolve; });
  const completion = (event, id) => {
    const key = `${event}:${id}`;
    if (!completions.has(key)) {
      if (completions.size >= 3072) throw Error('AW Qoder callback identity limit');
      let done;
      const promise = new Promise(resolve => { done = resolve; });
      completions.set(key, { promise, done });
    }
    return completions.get(key);
  };
  active = {
    async *input() {
      await ready;
      if (!failed) yield { type: 'user', parent_tool_use_id: null,
        message: { role: 'user', content: prompt } };
    },
    wait: (event, id) => completion(event, id).promise,
    complete: (event, id) => completion(event, id).done(),
    record(payload) {
      if (payload.hook_event_name === 'PreToolUse') {
        return payload.tool_name === 'Bash' && !payload.tool_input?.run_in_background;
      }
      const id = payload.tool_use_id;
      if (typeof id !== 'string' || !id || Object.keys(records).length >= 1024) throw Error('AW Qoder identity limit');
      const native = Object.fromEntries(['session_id', 'cwd', 'tool_use_id', 'tool_name', 'tool_input']
        .map(key => [key, payload[key]]));
      native.is_error = payload.hook_event_name === 'PostToolUseFailure';
      if (Object.hasOwn(records, id)) native.ambiguous = true;
      records[id] = JSON.parse(JSON.stringify(native));
      save();
      return true;
    },
    verify() {
      const runtime = JSON.parse(readFileSync(`${root}/qoder-final-ready.json`, 'utf8'));
      if (runtime.token !== config.ready_token || !isDeepStrictEqual(runtime.profile, config.final_profile)) {
        throw Error('AW Qoder final runtime readiness mismatch');
      }
    },
    release() { release(); },
    cancel() { failed = true; release(); },
  };
  return active;
}

export function queryFinal(config) { return config.final_profile ? active : undefined; }
