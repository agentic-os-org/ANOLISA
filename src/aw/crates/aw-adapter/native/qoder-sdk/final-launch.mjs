/** Keep the legacy final gate bound to one foreground Query and native identities. */
import { readFileSync } from 'node:fs';
import { dirname } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import { finalRecords } from './final-records.mjs';

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
  const enforcing = config.final_enforcing !== false;
  const root = dirname(config.ready_path), completions = new Map();
  let records = Object.create(null);
  const snapshots = finalRecords(root, 'qoder', enforcing);
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
    enforcing,
    prepare: () => snapshots.publish(records),
    close: () => snapshots.close(),
    async *input() {
      await ready;
      if (!failed) yield { type: 'user', parent_tool_use_id: null,
        message: { role: 'user', content: prompt } };
    },
    wait: (event, id) => completion(event, id).promise,
    complete: (event, id) => completion(event, id).done(),
    record(payload) {
      const supported = payload.tool_name === 'Bash' && !payload.tool_input?.run_in_background;
      if (payload.hook_event_name === 'PreToolUse') {
        return supported;
      }
      if (!enforcing && !supported) return;
      try {
        const id = payload.tool_use_id;
        if (typeof id !== 'string' || !id) throw Error('AW Qoder identity limit');
        const duplicate = Object.hasOwn(records, id);
        if (Object.keys(records).length >= 1024 && !(enforcing && duplicate)) throw Error('AW Qoder identity limit');
        const native = enforcing && duplicate ? records[id] : JSON.parse(JSON.stringify({
          ...Object.fromEntries(['session_id', 'cwd', 'tool_use_id', 'tool_name', 'tool_input']
            .map(key => [key, payload[key]])),
          is_error: payload.hook_event_name === 'PostToolUseFailure',
        }));
        const next = { ...records, [id]: duplicate ? { ...native, ambiguous: true } : native };
        if (enforcing) records = next;
        if (snapshots.publish(next)) records = next;
      } catch (error) {
        if (enforcing) throw error;
      }
      return true;
    },
    verify() {
      const runtime = JSON.parse(readFileSync(`${root}/qoder-final-ready.json`, 'utf8'));
      if (runtime.token !== config.ready_token || !isDeepStrictEqual(runtime.profile, config.final_profile)) {
        throw Error('AW Qoder final runtime readiness mismatch');
      }
    },
    release() { release(); },
    cancel() { snapshots.close(); failed = true; release(); },
  };
  return active;
}

export function queryFinal(config) { return config.final_profile ? active : undefined; }
