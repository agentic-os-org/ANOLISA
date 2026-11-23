import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";

import { LanguageSource, SourceDenied } from "../src/sources.mjs";
import {
  BINARY,
  readProviderLog,
  startAwService,
} from "./source-mutation-fixture.mjs";

const NATIVE_HOOK_SCRIPT = `import json, sys
req = json.load(sys.stdin)
cmd = str((req.get("input") or {}).get("command", ""))
if cmd == "native_block":
    sys.exit(2)
if cmd == "native_diag":
    sys.stdout.write("ordinary diagnostic stdout\\n")
    sys.exit(0)
if cmd == "native_forge_replace":
    sys.stdout.write(json.dumps({"aw_replacement": "FORGED"}) + "\\n")
    sys.exit(0)
if cmd == "native_forge_withhold":
    sys.stdout.write(json.dumps({"aw_withheld": True}) + "\\n")
    sys.exit(0)
sys.exit(0)
`;

const REPORT_PROVIDER_SCRIPT = `import json, sys
r = json.load(sys.stdin)
out = {"api_version": r["api_version"], "request_id": r["request_id"], "status": "ok"}
if r["method"] == "describe":
    out["operations"] = [
        {"name": "filter", "events": ["tool.after"], "effects": ["observe", "replace_result"]},
    ]
elif r["method"] == "invoke":
    ev = r["event"]
    out["input_digest"] = r["input_digest"]
    text = (ev["tool"].get("result_projection") or {}).get("text", "")
    if "FAULT" in text:
        sys.stderr.write("recoverable provider fault\\n")
        sys.exit(1)
    if "SECRET" in text:
        out["effects"] = [{"type": "replace_result", "text": text.replace("SECRET", "[REDACTED]")}]
    else:
        out["effects"] = []
print(json.dumps(out))
`;

async function startCustomService(root, { afterSteps, providers }) {
  const state = path.join(root, "s");
  mkdirSync(state, { mode: 0o700 });
  const config = {
    apiVersion: "aw/v1alpha1",
    kind: "AWConfiguration",
    metadata: { name: "layer1-edge-js" },
    spec: {
      daemon: { startup: "external", endpoint: "auto", state_dir: state },
      execution: { guarantee: "native_hook", default_event_budget_ms: 4000 },
      audit: { enabled: true, payload: "metadata_only" },
      agents: { app: { adapter: "sdk", argv: ["/bin/true"] } },
      providers,
      events: {
        "tool.after": {
          enabled: true,
          required: true,
          steps: afterSteps,
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

function makeDepth32Array() {
  let node = "leaf";
  for (let i = 0; i < 32; i++) node = [node];
  return node;
}

test(
  "Deep stream extractor isolation, replay AbortSignal, and empty-stream 1-byte budget",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awje1-"));
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
      await source.connect();
      try {
        await source.withSession({ sessionId: "js-edge-1" }, async (session) => {
          // P1-1: chunk 0 + depth-32 chunk; extractor mutates chunk 0 in place.
          const deep = makeDepth32Array();
          async function* deepAsync() {
            yield { text: "before" };
            yield deep;
          }
          function* deepSync() {
            yield { text: "before" };
            yield deep;
          }
          const mutatingExtract = (chunks) => {
            const audited = chunks[0].text;
            chunks[0].text = "after";
            return audited;
          };

          const wAsync = session.wrapToolStream(deepAsync, {
            name: "deep_async",
            bufferStream: true,
            extractResult: mutatingExtract,
          });
          const outAsync = [];
          for await (const c of wAsync({ command: "ok" })) outAsync.push(c);
          assert.deepEqual(outAsync[0], { text: "before" });
          assert.deepEqual(outAsync[1], deep);

          const wSync = session.wrapTool(deepSync, {
            name: "deep_sync",
            bufferStream: true,
            extractResult: mutatingExtract,
          });
          const outSync = [...(await wSync({ command: "ok" }))];
          assert.deepEqual(outSync[0], { text: "before" });
          assert.deepEqual(outSync[1], deep);

          // P2-5: Caller AbortSignal interrupts buffered replay between yielded chunks.
          async function* twoChunks() {
            yield "c1";
            yield "c2";
          }
          const abortable = session.wrapToolStream(twoChunks, {
            name: "abort_replay",
            bufferStream: true,
            extractSignal: (_args, sig) => sig,
            extractInput: (args) => args,
          });
          const ac = new AbortController();
          const iter = abortable({ command: "ok" }, ac.signal);
          const first = await iter.next();
          assert.deepEqual(first, { value: "c1", done: false });
          const reason = new Error("caller-cancelled-replay");
          ac.abort(reason);
          await assert.rejects(
            () => iter.next(),
            (err) => err === reason && err.toolExecuted === true,
          );

          // P2-6: Empty stream ('[]' = 2 bytes) rejected under 1-byte budget
          // before extractResult or tool.after runs.
          let extractCalls = 0;
          const zeroExtract = () => {
            extractCalls += 1;
            return 0;
          };
          async function* emptyAsync() {
            /* empty */
          }
          function* emptySync() {
            /* empty */
          }
          const afterBefore = readProviderLog(service.logPath).filter(
            (e) => e.event === "tool.after",
          ).length;

          const wEmptyA = session.wrapToolStream(emptyAsync, {
            name: "empty_a",
            bufferStream: true,
            maxStreamBytes: 1,
            extractResult: zeroExtract,
          });
          await assert.rejects(
            async () => {
              for await (const _ of wEmptyA({ command: "ok" })) {
                /* empty */
              }
            },
            (err) =>
              err?.code === "source_stream_result_too_large" &&
              err.toolExecuted === true,
          );

          const wEmptyS = session.wrapTool(emptySync, {
            name: "empty_s",
            bufferStream: true,
            maxStreamBytes: 1,
            extractResult: zeroExtract,
          });
          await assert.rejects(
            () => wEmptyS({ command: "ok" }),
            (err) =>
              err?.code === "source_stream_result_too_large" &&
              err.toolExecuted === true,
          );

          assert.equal(extractCalls, 0);
          const afterCount = readProviderLog(service.logPath).filter(
            (e) => e.event === "tool.after",
          ).length;
          assert.equal(afterCount, afterBefore);
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
  "Native hook stdout isolation, tool.after block zero-delivery, and on_error report recovery",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awje2-"));
    const nativeScript = path.join(root, "native_hook.py");
    writeFileSync(nativeScript, NATIVE_HOOK_SCRIPT);
    const provScript = path.join(root, "report_provider.py");
    writeFileSync(provScript, REPORT_PROVIDER_SCRIPT);
    const service = await startCustomService(root, {
      providers: {
        nh: {
          protocol: "native-hook/v1alpha1",
          transport: {
            type: "stdio",
            location: "agent",
            argv: ["python3", nativeScript],
          },
          timeout_ms: 2000,
          max_output_bytes: 4096,
          config: {},
        },
        p: {
          protocol: "aw-provider/v1alpha1",
          transport: {
            type: "stdio",
            location: "agent",
            argv: ["python3", provScript],
          },
          timeout_ms: 2000,
          max_output_bytes: 65536,
          config: {},
        },
      },
      afterSteps: [
        { id: "native", provider: "nh", native: {}, on_error: "report" },
        {
          id: "flt",
          provider: "p",
          operation: "filter",
          effects: ["observe", "replace_result"],
          on_error: "report",
        },
      ],
    });
    try {
      const source = LanguageSource.create({
        endpoint: service.endpoint,
        target: "app",
        availability: "required",
        replaceResult: true,
      });
      await source.connect();
      try {
        await source.withSession({ sessionId: "js-edge-2" }, async (session) => {
          const wEcho = session.wrapTool(async (args) => args.out, {
            name: "echo",
          });
          const wStream = session.wrapToolStream(
            async function* (args) {
              yield args.out;
            },
            { name: "echo_stream", bufferStream: true },
          );

          // P1-3a: Native hook exiting 0 with plain diagnostic stdout passes cleanly.
          assert.equal(
            await wEcho({ command: "native_diag", out: "plain" }),
            "plain",
          );

          // P1-3b: Native hook stdout cannot forge replacement or withholding.
          assert.equal(
            await wEcho({ command: "native_forge_replace", out: "real" }),
            "real",
          );
          assert.equal(
            await wEcho({ command: "native_forge_withhold", out: "real" }),
            "real",
          );

          // P1-2: tool.after returning block (native exit 2) yields zero output
          // and closes the event cleanly so subsequent calls succeed.
          await assert.rejects(
            () => wEcho({ command: "native_block", out: "sample" }),
            (err) =>
              err instanceof SourceDenied &&
              err.event === "tool.after" &&
              err.toolExecuted === true,
          );

          const leaked = [];
          await assert.rejects(
            async () => {
              for await (const c of wStream({
                command: "native_block",
                out: "sample",
              }))
                leaked.push(c);
            },
            (err) =>
              err instanceof SourceDenied &&
              err.event === "tool.after" &&
              err.toolExecuted === true,
          );
          assert.deepEqual(leaked, []);

          // P2-4: replace_result with on_error: report recovers on provider fault
          // and preserves real replacement when provider succeeds.
          assert.equal(
            await wEcho({ command: "ok", out: "FAULT-payload" }),
            "FAULT-payload",
          );
          assert.equal(
            await wEcho({ command: "ok", out: "has SECRET" }),
            "has [REDACTED]",
          );
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
  "Native tool.after block honors explicit async buffering",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awje-"));
    const hook = path.join(root, "native_hook.py");
    writeFileSync(hook, NATIVE_HOOK_SCRIPT);
    const service = await startCustomService(root, {
      providers: {
        nh: {
          protocol: "native-hook/v1alpha1",
          transport: {
            type: "stdio",
            location: "agent",
            argv: ["python3", hook],
          },
          timeout_ms: 2000,
          max_output_bytes: 4096,
          config: {},
        },
      },
      afterSteps: [
        { id: "native", provider: "nh", native: {}, on_error: "report" },
      ],
    });
    try {
      const source = LanguageSource.create({
        endpoint: service.endpoint,
        target: "app",
        availability: "required",
      });
      await source.connect();
      try {
        await source.withSession(
          { sessionId: "js-native-buffer" },
          async (session) => {
            await session.prepare();
            assert.equal(session._observeAfter(), true);
            assert.equal(session._strictAfter(), false);

            async function* chunks(args) {
              yield args.out;
            }

            for (const bufferStream of [true, false]) {
              const wrapped = session.wrapToolStream(chunks, { bufferStream });
              const delivered = [];
              await assert.rejects(
                async () => {
                  for await (const chunk of wrapped({
                    command: "native_block",
                    out: "sample",
                  })) {
                    delivered.push(chunk);
                  }
                },
                (err) =>
                  err instanceof SourceDenied &&
                  err.event === "tool.after" &&
                  err.toolExecuted === true,
              );
              assert.deepEqual(delivered, bufferStream ? [] : ["sample"]);
            }
          },
        );
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
  "Observe-only route with bufferStream: true passes chunk arrays to extractResult in wrapTool and instrument",
  { skip: !BINARY, timeout: 25000 },
  async () => {
    const root = mkdtempSync(path.join("/tmp", "awje3-"));
    const logPath = path.join(root, "obs.jsonl");
    const obsScript = path.join(root, "obs_provider.py");
    writeFileSync(
      obsScript,
      `import json, sys
LOG = ${JSON.stringify(logPath)}
r = json.load(sys.stdin)
out = {"api_version": r["api_version"], "request_id": r["request_id"], "status": "ok"}
if r["method"] == "describe":
    out["operations"] = [{"name": "watch", "events": ["tool.after"], "effects": ["observe"]}]
elif r["method"] == "invoke":
    ev = r["event"]
    out["input_digest"] = r["input_digest"]
    with open(LOG, "a", encoding="utf-8") as fh:
        fh.write(json.dumps({"event": ev["name"], "tool": ev.get("tool")}) + "\\n")
    out["effects"] = []
print(json.dumps(out))
`,
    );
    const service = await startCustomService(root, {
      providers: {
        obs: {
          protocol: "aw-provider/v1alpha1",
          transport: {
            type: "stdio",
            location: "agent",
            argv: ["python3", obsScript],
          },
          timeout_ms: 2000,
          max_output_bytes: 65536,
          config: {},
        },
      },
      afterSteps: [
        {
          id: "watch",
          provider: "obs",
          operation: "watch",
          effects: ["observe"],
          on_error: "report",
        },
      ],
    });
    try {
      const source = LanguageSource.create({
        endpoint: service.endpoint,
        target: "app",
        availability: "required",
      });
      await source.connect();
      try {
        await source.withSession({ sessionId: "js-obs-buf" }, async (session) => {
          await session.prepare();
          assert.equal(session._strictAfter(), false);
          assert.equal(session._observeAfter(), true);
          const extracted = [];
          function* syncChunks() {
            yield { text: "alpha" };
            yield { text: "beta" };
          }
          const wrapped = session.wrapTool(syncChunks, {
            name: "obs_wrap",
            bufferStream: true,
            extractResult: (chunks) => {
              extracted.push(["obs_wrap", chunks]);
              return chunks.map((c) => c.text).join(",");
            },
          });
          const deliveredWrap = [...(await wrapped({ command: "ok" }))];
          assert.deepEqual(deliveredWrap, [{ text: "alpha" }, { text: "beta" }]);

          const target = { syncChunks };
          const inst = session.instrument(target, {
            toolMethods: {
              syncChunks: {
                name: "obs_inst",
                bufferStream: true,
                extractResult: (chunks) => {
                  extracted.push(["obs_inst", chunks]);
                  return chunks.map((c) => c.text).join(",");
                },
              },
            },
          });
          try {
            const deliveredInst = [...(await target.syncChunks({ command: "ok" }))];
            assert.deepEqual(deliveredInst, [{ text: "alpha" }, { text: "beta" }]);
          } finally {
            inst.restore();
          }

          assert.deepEqual(extracted, [
            ["obs_wrap", [{ text: "alpha" }, { text: "beta" }]],
            ["obs_inst", [{ text: "alpha" }, { text: "beta" }]],
          ]);
          const observed = readProviderLog(logPath).filter(
            (e) => e.event === "tool.after",
          );
          assert.equal(observed.length, 2);
          assert.deepEqual(
            observed.map((e) => [
              e.tool.name,
              e.tool.result ?? e.tool.result_projection?.text,
            ]),
            [
              ["obs_wrap", "alpha,beta"],
              ["obs_inst", "alpha,beta"],
            ],
          );
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
