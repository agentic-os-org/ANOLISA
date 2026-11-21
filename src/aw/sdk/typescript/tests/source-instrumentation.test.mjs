import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, existsSync } from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import {
  LanguageSource,
  SourceDenied,
  SourceResultWithheld,
  confirmSourceRoutes,
  sourceCapabilities,
} from "../src/sources.mjs";
import {
  installAutoInstrumentation,
  loadSpecFromEnv,
  validateAutoSpec,
} from "../src/source-preload.mjs";

const BINARY = process.env.AW_TEST_BINARY;
const PRELOAD_PATH = fileURLToPath(
  new URL("../src/source-preload.mjs", import.meta.url),
);

const PROVIDER_SCRIPT = `import json, sys
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
    if op == 'rewrite':
        inp = dict(ev['tool']['input'])
        cmd = inp.get('command')
        url = inp.get('url')
        if isinstance(cmd, str) and cmd.startswith('raw:'):
            inp['command'] = 'safe:' + cmd[4:]
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        elif isinstance(cmd, str) and cmd.startswith('rewrite-to-deny'):
            inp['command'] = 'blocked'
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        elif isinstance(url, str) and url.endswith('/raw'):
            inp['url'] = url[:-4] + '/rewritten'
            out['effects'] = [{'type': 'replace_input', 'input': inp}]
        else:
            out['effects'] = []
    elif op == 'guard':
        deny = (
            ev.get('input', {}).get('text') == 'deny'
            or ev.get('tool', {}).get('input', {}).get('command') == 'blocked'
            or str(ev.get('tool', {}).get('input', {}).get('url', '')).endswith('/deny')
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

async function startAwService(root) {
  const state = path.join(root, "s");
  mkdirSync(state, { mode: 0o700 });
  const provider = path.join(root, "provider.py");
  writeFileSync(provider, PROVIDER_SCRIPT);
  const config = {
    apiVersion: "aw/v1alpha1",
    kind: "AWConfiguration",
    metadata: { name: "layer1-layer2-js" },
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
            argv: ["python3", provider],
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
    async stop() {
      child.kill("SIGTERM");
      await new Promise((resolve) => child.once("exit", resolve));
    },
  };
}

test("opt-in sourceCapabilities and auto preload spec validation", () => {
  const base = sourceCapabilities(["tool.before", "tool.after"]);
  assert.equal(base.failure_actions, undefined);
  assert.equal(
    confirmSourceRoutes(base, {
      hooks: [{ event: "tool.after", on_error: "withhold_result" }],
    }),
    false,
  );
  const full = sourceCapabilities(["input.submit", "tool.before", "tool.after"], {
    replaceInput: true,
    replaceResult: true,
    withholdResult: true,
  });
  assert.ok(full.events["tool.before"].includes("replace_input"));
  assert.ok(full.events["tool.after"].includes("replace_result"));
  assert.deepEqual(full.failure_actions, { "tool.after": ["withhold_result"] });
  assert.equal(
    confirmSourceRoutes(full, {
      hooks: [
        { event: "tool.before", replace_input: true },
        {
          event: "tool.after",
          replace_result: true,
          on_error: "withhold_result",
        },
      ],
    }),
    true,
  );
  assert.equal(loadSpecFromEnv({}), null);
  const valid = validateAutoSpec({
    endpoint: "/tmp/aw.sock",
    target: "app",
    targets: [{ kind: "fetch" }],
  });
  assert.equal(valid.targets[0].name, "fetch");
  for (const bad of [
    { endpoint: "/tmp/aw.sock", target: "app", targets: [] },
    { endpoint: "/tmp/aw.sock", target: "app", extra: 1, targets: [{ kind: "fetch" }] },
    {
      endpoint: "/tmp/aw.sock",
      target: "app",
      targets: [{ kind: "module", module: "./*.mjs", exportName: "f" }],
    },
    {
      endpoint: "/tmp/aw.sock",
      target: "app",
      targets: [{ kind: "module", module: "./m.mjs", exportName: "bad-name" }],
    },
  ]) {
    assert.throws(() => validateAutoSpec(bad), {
      code: "source_auto_config_invalid",
    });
  }
});

test(
  "Layer 1 rewrite, withhold and streaming boundaries against real AW service",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awj1-"));
    const service = await startAwService(root);
    try {
      const source = LanguageSource.create({
        endpoint: service.endpoint,
        target: "app",
        availability: "required",
        replaceInput: true,
        replaceResult: true,
        withholdResult: true,
      });
      try {
        await source.connect();
        await source.withSession({ sessionId: "js-layer1" }, async (session) => {
          const executed = [];
          const wrapped = session.wrapTool(
            async (args) => {
              executed.push(args.command);
              return `out:${args.command}`;
            },
            { name: "shell" },
          );

          assert.equal(await wrapped({ command: "raw:hello" }), "out:safe:hello");
          assert.deepEqual(executed, ["safe:hello"]);

          await assert.rejects(
            wrapped({ command: "rewrite-to-deny" }),
            (err) => err instanceof SourceDenied && err.event === "tool.before",
          );
          assert.deepEqual(executed, ["safe:hello"]);

          assert.equal(await wrapped({ command: "SECRET" }), "out:[REDACTED]");

          const wrappedStruct = session.wrapTool(
            async (args) => ({ stdout: `data:${args.command}`, exit_code: 0 }),
            { name: "struct" },
          );
          assert.deepEqual(await wrappedStruct({ command: "SECRET" }), {
            stdout: "data:[REDACTED]",
            exit_code: 0,
          });

          await assert.rejects(
            wrapped({ command: "FAULT" }),
            (err) => err instanceof SourceResultWithheld && err.toolExecuted === true,
          );

          // Custom extractInput without applyInput rejects replacement (never silent drop).
          class Req {
            constructor(command) {
              this.command = command;
            }
          }
          const noApply = session.wrapTool(async (r) => r.command, {
            name: "obj",
            extractInput: (r) => ({ command: r.command }),
            snapshotArg: (r) => new Req(r.command),
          });
          await assert.rejects(noApply(new Req("raw:bad")), {
            code: "source_input_replace_unsupported",
          });
          const withApply = session.wrapTool(async (r) => r.command, {
            name: "obj",
            extractInput: (r) => ({ command: r.command }),
            applyInput: (u) => [new Req(u.command)],
            snapshotArg: (r) => new Req(r.command),
          });
          assert.equal(await withApply(new Req("raw:ok")), "safe:ok");

          // Honest streaming boundary: unbuffered rejected before native stream starts.
          let streamRuns = 0;
          async function* streamTool(args) {
            streamRuns++;
            yield `chunk:${args.command}`;
          }
          const unbuffered = session.wrapToolStream(streamTool, { name: "stream" });
          await assert.rejects(
            async () => {
              for await (const _ of unbuffered({ command: "ok" })) {
                /* empty */
              }
            },
            { code: "source_stream_withhold_requires_buffering" },
          );
          assert.equal(streamRuns, 0);

          // Buffered stream replaces and withholds with zero leaked chunks.
          const buffered = session.wrapToolStream(streamTool, {
            name: "stream",
            bufferStream: true,
            extractResult: (chunks) => chunks.join("\n"),
            applyResult: (_orig, rep) => rep.split("\n"),
          });
          const replaced = [];
          for await (const c of buffered({ command: "SECRET" })) replaced.push(c);
          assert.deepEqual(replaced, ["chunk:[REDACTED]"]);

          const leaked = [];
          await assert.rejects(
            async () => {
              for await (const c of buffered({ command: "FAULT" })) leaked.push(c);
            },
            (err) => err instanceof SourceResultWithheld,
          );
          assert.deepEqual(leaked, []);
        });
      } finally {
        await source.close();
      }
    } finally {
      await service.stop();
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "Layer 2 preload instruments globalThis.fetch and --import ESM exports with clean uninstall",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awj2-"));
    const service = await startAwService(root);
    const seenPaths = [];
    const server = http.createServer((req, res) => {
      seenPaths.push(req.url);
      if (req.url === "/secret") res.end("token=SECRET");
      else if (req.url === "/fault") res.end("trigger=FAULT");
      else res.end(`path:${req.url}`);
    });
    await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
    const { port } = server.address();
    const baseUrl = `http://127.0.0.1:${port}`;
    try {
      // 1. globalThis.fetch instrumentation (block, rewrite URL, redact body, withhold body, restore).
      const handle = installAutoInstrumentation({
        endpoint: service.endpoint,
        target: "app",
        availability: "required",
        replaceInput: true,
        replaceResult: true,
        withholdResult: true,
        targets: [{ kind: "fetch", name: "http_fetch" }],
      });
      try {
        await assert.rejects(
          fetch(`${baseUrl}/deny`),
          (err) => err instanceof SourceDenied && err.event === "tool.before",
        );
        assert.deepEqual(seenPaths, []);

        const r1 = await fetch(`${baseUrl}/raw`);
        assert.equal(await r1.text(), "path:/rewritten");
        assert.deepEqual(seenPaths, ["/rewritten"]);

        const r2 = await fetch(`${baseUrl}/secret`);
        assert.equal(await r2.text(), "token=[REDACTED]");

        await assert.rejects(
          fetch(`${baseUrl}/fault`),
          (err) => err instanceof SourceResultWithheld,
        );
      } finally {
        await handle.uninstall();
      }
      // After uninstall, fetch runs natively without AW.
      const nativeRes = await fetch(`${baseUrl}/secret`);
      assert.equal(await nativeRes.text(), "token=SECRET");

      // 2. Zero-code-change `node --import .../source-preload.mjs app.mjs` on ESM module exports.
      const toolsFile = path.join(root, "tools.mjs");
      writeFileSync(
        toolsFile,
        `export const UNTOUCHED = 42;\nexport async function runCmd(args) { return "mod:" + args.command; }\n`,
      );
      const appFile = path.join(root, "app.mjs");
      writeFileSync(
        appFile,
        `import { UNTOUCHED, runCmd } from ${JSON.stringify(toolsFile)};\n` +
          `console.log(UNTOUCHED);\n` +
          `console.log(await runCmd({ command: "raw:preload" }));\n` +
          `console.log(await runCmd({ command: "SECRET" }));\n`,
      );
      const specFile = path.join(root, "preload.json");
      writeFileSync(
        specFile,
        JSON.stringify({
          endpoint: service.endpoint,
          target: "app",
          availability: "required",
          replaceInput: true,
          replaceResult: true,
          withholdResult: true,
          targets: [
            {
              kind: "module",
              module: toolsFile,
              exportName: "runCmd",
              role: "tool",
              name: "shell",
            },
          ],
        }),
      );
      const res = spawnSync(
        process.execPath,
        ["--import", PRELOAD_PATH, appFile],
        {
          encoding: "utf8",
          timeout: 15000,
          env: {
            ...process.env,
            AW_AUTO_INSTRUMENT: "1",
            AW_AUTO_INSTRUMENT_CONFIG: specFile,
          },
        },
      );
      assert.equal(res.status, 0, res.stdout + res.stderr);
      assert.deepEqual(res.stdout.trim().split("\n"), [
        "42",
        "mod:safe:preload",
        "mod:[REDACTED]",
      ]);
    } finally {
      await new Promise((resolve) => server.close(resolve));
      await service.stop();
      rmSync(root, { recursive: true, force: true });
    }
  },
);
