import { spawn } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { setTimeout as delay } from "node:timers/promises";

export const BINARY = process.env.AW_TEST_BINARY;

const PROVIDER_SCRIPT = `import json, sys
from pathlib import Path
LOG = Path(sys.argv[1])
r = json.load(sys.stdin)
out = {'api_version': r['api_version'], 'request_id': r['request_id'], 'status': 'ok'}
if r['method'] == 'describe':
    out['operations'] = [
        {'name': 'rewrite', 'events': ['tool.before'], 'effects': ['replace_input']},
        {'name': 'guard', 'events': ['input.submit', 'tool.before'], 'effects': ['observe', 'block']},
        {'name': 'filter', 'events': ['tool.after'], 'effects': ['observe', 'replace_result']},
    ]
elif r['method'] == 'invoke':
    ev = r['event']
    op = r['operation']
    out['input_digest'] = r['input_digest']
    with LOG.open('a', encoding='utf-8') as fh:
        fh.write(json.dumps({'event': ev['name'], 'op': op, 'tool': ev.get('tool', {}).get('name')}) + '\\n')
    if op == 'rewrite':
        inp = dict(ev['tool']['input'])
        cmd = inp.get('command')
        if isinstance(cmd, str) and cmd.startswith('raw:'):
            inp['command'] = 'safe:' + cmd[4:]
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        elif isinstance(cmd, str) and cmd.startswith('rewrite-to-deny'):
            inp['command'] = 'blocked'
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        else:
            out['effects'] = []
    elif op == 'guard':
        deny = (
            ev.get('input', {}).get('text') == 'deny'
            or ev.get('tool', {}).get('input', {}).get('command') == 'blocked'
        )
        out['effects'] = [{'type': 'block', 'reason_code': 'policy_denied'}] if deny else []
    elif op == 'filter':
        proj = ev['tool'].get('result_projection') or {}
        text = proj.get('text', '')
        if 'FAULT' in text:
            sys.stderr.write('simulated scanner failure\\n')
            sys.exit(1)
        if 'SECRET' in text:
            out['effects'] = [{'type': 'replace_result', 'text': text.replace('SECRET', '[REDACTED]')}]
        else:
            out['effects'] = []
print(json.dumps(out))
`;

export async function startAwService(root) {
  const state = path.join(root, "s");
  mkdirSync(state, { mode: 0o700 });
  const provider = path.join(root, "provider.py");
  const logPath = path.join(root, "provider.jsonl");
  writeFileSync(provider, PROVIDER_SCRIPT);
  const config = {
    apiVersion: "aw/v1alpha1",
    kind: "AWConfiguration",
    metadata: { name: "layer1-mutation-js" },
    spec: {
      daemon: { startup: "external", endpoint: "auto", state_dir: state },
      execution: { guarantee: "native_hook", default_event_budget_ms: 4000 },
      audit: { enabled: true, payload: "metadata_only" },
      agents: { app: { adapter: "sdk", argv: ["/bin/true"] } },
      providers: {
        p: {
          protocol: "aw-provider/v1alpha1",
          transport: {
            type: "stdio",
            location: "agent",
            argv: ["python3", provider, logPath],
          },
          timeout_ms: 2000,
          max_output_bytes: 65536,
          config: {},
        },
      },
      events: {
        "input.submit": {
          enabled: true,
          required: true,
          steps: [
            {
              id: "g",
              provider: "p",
              operation: "guard",
              effects: ["observe", "block"],
              on_error: "block",
            },
          ],
        },
        "tool.before": {
          enabled: true,
          required: true,
          steps: [
            {
              id: "rw",
              provider: "p",
              operation: "rewrite",
              effects: ["replace_input"],
              on_error: "block",
            },
            {
              id: "g",
              provider: "p",
              operation: "guard",
              effects: ["observe", "block"],
              on_error: "block",
            },
          ],
        },
        "tool.after": {
          enabled: true,
          required: true,
          steps: [
            {
              id: "flt",
              provider: "p",
              operation: "filter",
              effects: ["observe", "replace_result"],
              on_error: "withhold_result",
            },
          ],
        },
      },
    },
  };
  const cfgPath = path.join(root, "aw.json");
  writeFileSync(cfgPath, JSON.stringify(config));
  const endpoint = path.join(state, "aw.sock");
  const child = spawn(
    BINARY,
    ["service", "serve", "--config", cfgPath, "--state-dir", state],
    { stdio: "ignore" },
  );
  const deadline = Date.now() + 10000;
  while (!existsSync(endpoint)) {
    if (child.exitCode !== null || Date.now() > deadline)
      throw new Error("aw service failed to start");
    await delay(10);
  }
  return {
    endpoint,
    logPath,
    async stop() {
      child.kill("SIGTERM");
      await new Promise((resolve) => child.once("exit", resolve));
    },
  };
}

export function readProviderLog(logPath) {
  if (!existsSync(logPath)) return [];
  return readFileSync(logPath, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}
