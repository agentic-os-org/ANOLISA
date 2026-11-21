import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";

import { LanguageSource, SourceDenied } from "../src/sources.mjs";
import { installAutoInstrumentation } from "../src/source-preload.mjs";

const AW_BINARY = process.env.AW_TEST_BINARY;

function writeHardeningProvider(root, logPath) {
  const script = path.join(root, "hardening_provider.py");
  writeFileSync(
    script,
    `#!/usr/bin/env python3
import json, sys
from pathlib import Path
LOG = Path(${JSON.stringify(logPath)})
req = json.loads(sys.stdin.read())
m = req["method"]
if m == "describe":
    out = {"api_version":"aw-provider/v1alpha1","request_id":req["request_id"],"status":"ok","operations":[
        {"name":"rewrite","events":["tool.before"],"effects":["replace_input"]},
        {"name":"before","events":["tool.before"],"effects":["observe","block"]},
        {"name":"after","events":["tool.after"],"effects":["observe","replace_result"]},
    ]}
elif m == "validate_config":
    out = {"api_version":"aw-provider/v1alpha1","request_id":req["request_id"],"status":"ok"}
else:
    ev = req["event"]
    name = ev["name"]
    op = req["operation"]
    tool = ev["tool"]
    cfg = req.get("config", {})
    with LOG.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps({"event": name, "op": op, "tool": tool["name"], "input": tool.get("input"), "result": tool.get("result")}) + "\\n")
    effects = []
    if op == "rewrite":
        inp = tool.get("input") or {}
        cmd = str(inp.get("command", ""))
        url = str(inp.get("url") or "")
        if "BAD_METHOD" in url:
            effects.append({"type":"replace_input","input":{**inp, "method": 123}})
        elif "BAD_HEADERS" in url:
            effects.append({"type":"replace_input","input":{**inp, "headers": {"x-bad": 42}}})
        elif "BAD_GET_BODY" in url:
            effects.append({"type":"replace_input","input":{**inp, "method": "GET", "body": "illegal"}})
        elif "REWRITE_FETCH" in url:
            new_url = url.replace("REWRITE_FETCH", "rewritten")
            effects.append({"type":"replace_input","input":{**inp, "url": new_url, "body": "rewritten-body"}})
        elif "REWRITE" in cmd:
            effects.append({"type":"replace_input","input":{**inp, "command": "safe"}})
    elif op == "before":
        inp = tool.get("input") or {}
        cmd = str(inp.get("command", ""))
        body = str(inp.get("body") or "")
        url = str(inp.get("url") or "")
        if "BLOCK" in cmd or "BLOCK" in body or "BLOCK" in url or cfg.get("mode") == "block_all":
            effects.append({"type":"block","reason_code":"policy_denied"})
    elif op == "after":
        res = tool.get("result")
        proj = tool.get("result_projection") or {}
        text = proj.get("text", "")
        tag = cfg.get("replace_tag")
        if tag:
            if isinstance(text, str) and text:
                effects.append({"type":"replace_result","text": f"{tag}:{text}"})
        elif isinstance(res, dict) and res.get("status") == 204 and "FORCE_204_BODY" in str((tool.get("input") or {}).get("url", "")):
            effects.append({"type":"replace_result","text":"illegal-204-body"})
        elif isinstance(text, str) and "SECRET" in text:
            effects.append({"type":"replace_result","text": text.replace("SECRET", "[REDACTED]")})
    out = {"api_version":"aw-provider/v1alpha1","request_id":req["request_id"],"status":"ok","input_digest":req["input_digest"],"effects":effects}
sys.stdout.write(json.dumps(out))
`,
    "utf8",
  );
  chmodSync(script, 0o700);
  return script;
}

function writeConfig(root, provider, name, providerCfg = {}, afterStrict = true) {
  const cfg = path.join(root, `${name}.json`);
  const stateDir = path.join(root, `state-${name}`);
  mkdirSync(stateDir, { recursive: true, mode: 0o700 });
  const beforeSteps = afterStrict
    ? [
        { id: "rw", provider: "p", operation: "rewrite", effects: ["replace_input"], on_error: "block" },
        { id: "guard", provider: "p", operation: "before", effects: ["observe", "block"], on_error: "block" },
      ]
    : [{ id: "guard", provider: "p", operation: "before", effects: ["observe", "block"], on_error: "block" }];
  const afterStep = afterStrict
    ? { id: "guard", provider: "p", operation: "after", effects: ["observe", "replace_result"], on_error: "withhold_result" }
    : { id: "guard", provider: "p", operation: "after", effects: ["observe"], on_error: "report" };
  writeFileSync(
    cfg,
    JSON.stringify({
      apiVersion: "aw/v1alpha1",
      kind: "AWConfiguration",
      metadata: { name: `hardening-${name}` },
      spec: {
        daemon: { startup: "external", endpoint: "auto", state_dir: stateDir },
        execution: { guarantee: "native_hook", default_event_budget_ms: 4000 },
        audit: { enabled: true, payload: "metadata_only" },
        agents: { app: { adapter: "sdk", argv: ["/bin/true"] } },
        providers: {
          p: {
            protocol: "aw-provider/v1alpha1",
            transport: { type: "stdio", location: "agent", argv: [provider] },
            timeout_ms: 4000,
            max_output_bytes: 16384,
            config: providerCfg,
          },
        },
        events: {
          "tool.before": {
            enabled: true,
            required: true,
            steps: beforeSteps,
          },
          "tool.after": { enabled: true, required: true, steps: [afterStep] },
        },
      },
    }),
    "utf8",
  );
  return { cfg, stateDir };
}

async function withService(cfgObj, fn) {
  const { cfg: cfgPath, stateDir } = cfgObj;
  const sock = path.join(stateDir, "aw.sock");
  const child = spawn(
    AW_BINARY,
    ["service", "serve", "--config", cfgPath, "--state-dir", stateDir],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  let stderr = "";
  child.stderr.on("data", (d) => (stderr += d.toString()));
  const deadline = Date.now() + 10_000;
  while (!existsSync(sock)) {
    if (child.exitCode !== null) throw new Error(`aw service exited ${child.exitCode}: ${stderr}`);
    if (Date.now() > deadline) throw new Error(`service socket timeout: ${stderr}`);
    await new Promise((r) => setTimeout(r, 25));
  }
  try {
    return await fn(sock);
  } finally {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
}

function readLog(logPath) {
  if (!existsSync(logPath)) return [];
  return readFileSync(logPath, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

test(
  "P0-1 & P1-1: Fetch enforces lossless request consistency, preserves 204/binary/metadata, and fails closed on bad shapes",
  { skip: !AW_BINARY },
  async () => {
    const root = mkdtempSync(path.join(tmpdir(), "aw-js-fetch-"));
    const logPath = path.join(root, "audit.jsonl");
    const provider = writeHardeningProvider(root, logPath);
    const strictCfg = writeConfig(root, provider, "strict", {}, true);
    const obsCfg = writeConfig(root, provider, "obs", {}, false);

    const serverHits = [];
    const server = http.createServer((req, res) => {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => {
        const body = Buffer.concat(chunks).toString("utf8");
        serverHits.push({ url: req.url, method: req.method, headers: req.headers, body });
        if (req.url.startsWith("/204")) {
          res.writeHead(204);
          res.end();
        } else if (req.url === "/binary") {
          res.writeHead(200, { "content-type": "application/octet-stream" });
          res.end(Buffer.from([0xff, 0xfe]));
        } else if (req.url === "/redirect") {
          res.writeHead(302, { location: "/final" });
          res.end();
        } else if (req.url === "/final") {
          res.writeHead(200, { "content-type": "text/plain" });
          res.end("final-ok");
        } else if (req.url === "/slow") {
          setTimeout(() => {
            if (!res.writableEnded) {
              res.writeHead(200);
              res.end("slow-done");
            }
          }, 500);
        } else if (req.url === "/secret") {
          res.writeHead(200, { "content-type": "text/plain", "content-encoding": "identity" });
          res.end("SECRET-data");
        } else {
          res.writeHead(200, { "content-type": "text/plain" });
          res.end(`echo:${req.url}:${body}`);
        }
      });
    });
    await new Promise((r) => server.listen(0, "127.0.0.1", r));
    const base = `http://127.0.0.1:${server.address().port}`;

    try {
      // 1) Observe-only mode: 204, binary [0xff, 0xfe], and redirect URL/redirected preserved losslessly.
      await withService(obsCfg, async (sock) => {
        const handle = installAutoInstrumentation({
          endpoint: sock,
          target: "app",
          availability: "required",
          targets: [{ kind: "fetch", name: "http_fetch" }],
        });
        try {
          const r204 = await fetch(`${base}/204`);
          assert.equal(r204.status, 204);
          assert.equal(r204.body, null);

          const rBin = await fetch(`${base}/binary`);
          const bytes = Buffer.from(await rBin.arrayBuffer());
          assert.deepEqual([...bytes], [0xff, 0xfe]);

          const rRedir = await fetch(`${base}/redirect`);
          assert.equal(rRedir.redirected, true);
          assert.equal(rRedir.url, `${base}/final`);
          assert.equal(await rRedir.text(), "final-ok");
        } finally {
          await handle.uninstall();
        }
      });

      // 2) Strict mode: Request body blocking, header override semantics, init mutation immunity,
      //    AbortSignal preservation on replace_input, bad input/replace shapes fail closed.
      await withService(strictCfg, async (sock) => {
        const handle = installAutoInstrumentation({
          endpoint: sock,
          target: "app",
          availability: "required",
          replaceInput: true,
          replaceResult: true,
          withholdResult: true,
          targets: [{ kind: "fetch", name: "http_fetch" }],
        });
        try {
          // Request object with body -> policy sees body and blocks BEFORE hitting server.
          const hitsBefore = serverHits.length;
          await assert.rejects(
            () => fetch(new Request(`${base}/ok`, { method: "POST", body: "BLOCK_SECRET" })),
            SourceDenied,
          );
          assert.equal(serverHits.length, hitsBefore);

          // Request + init.headers -> Fetch override semantics (NOT merged!).
          const reqWithHeaders = new Request(`${base}/hdr`, {
            method: "POST",
            headers: { "x-orig": "1", "x-keep": "old" },
            body: "orig-body",
          });
          const respHdr = await fetch(reqWithHeaders, {
            headers: { "x-override": "2" },
          });
          assert.equal(await respHdr.text(), "echo:/hdr:orig-body");
          const lastHit = serverHits.at(-1);
          assert.equal(lastHit.headers["x-override"], "2");
          assert.equal(lastHit.headers["x-orig"], undefined);
          const beforeLogs = readLog(logPath).filter(
            (e) => e.event === "tool.before" && e.input?.url === `${base}/hdr`,
          );
          const auditedHdrs = beforeLogs.at(-1).input.headers;
          assert.equal(auditedHdrs["x-override"], "2");
          assert.equal(auditedHdrs["x-orig"], undefined);
          assert.equal(beforeLogs.at(-1).input.body, "orig-body");

          // Caller mutating init during beforeTool cannot change sent request.
          const mutInit = { method: "POST", headers: { "x-tag": "clean" }, body: "clean-body" };
          const p = fetch(`${base}/mut`, mutInit);
          mutInit.headers["x-tag"] = "TAMPERED";
          mutInit.body = "TAMPERED";
          const rMut = await p;
          assert.equal(await rMut.text(), "echo:/mut:clean-body");
          assert.equal(serverHits.at(-1).headers["x-tag"], "clean");

          // replace_input on Request preserves AbortSignal AND rewrites URL/body.
          const rRewrite = await fetch(
            new Request(`${base}/REWRITE_FETCH`, { method: "POST", body: "orig" }),
          );
          assert.equal(await rRewrite.text(), "echo:/rewritten:rewritten-body");

          const ac = new AbortController();
          ac.abort(new Error("caller-aborted"));
          await assert.rejects(
            () => fetch(new Request(`${base}/REWRITE_FETCH`, { method: "POST", body: "orig", signal: ac.signal })),
            /caller-aborted|AbortError/,
          );

          // Unsupported request body (ReadableStream) fails closed before hitting server.
          const hitsStream = serverHits.length;
          const rs = new ReadableStream({
            start(ctrl) {
              ctrl.enqueue(new Uint8Array([1, 2, 3]));
              ctrl.close();
            },
          });
          await assert.rejects(
            () => fetch(`${base}/stream`, { method: "POST", body: rs, duplex: "half" }),
            { code: "source_input_snapshot_required" },
          );
          assert.equal(serverHits.length, hitsStream);

          // Unsupported replace_input shapes fail closed with zero network hits.
          for (const badPath of ["/BAD_METHOD", "/BAD_HEADERS", "/BAD_GET_BODY"]) {
            const h0 = serverHits.length;
            await assert.rejects(
              () => fetch(`${base}${badPath}`, { method: "POST", body: "x" }),
              { code: "source_input_replace_unsupported" },
            );
            assert.equal(serverHits.length, h0);
          }

          // Strict replace_result on text works; illegal body replacement on 204 fails closed.
          const rSecret = await fetch(`${base}/secret`);
          assert.equal(await rSecret.text(), "[REDACTED]-data");

          await assert.rejects(
            () => fetch(`${base}/204?FORCE_204_BODY`),
            (err) => err?.code === "source_result_replace_unsupported" && err.toolExecuted === true,
          );
        } finally {
          await handle.uninstall();
        }
      });
    } finally {
      server.close();
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "P0-2 & P1-3: Strict output isolation, lazy iterators and cumulative stream byte budgets",
  { skip: !AW_BINARY },
  async () => {
    const root = mkdtempSync(path.join(tmpdir(), "aw-js-iso-"));
    const logPath = path.join(root, "audit.jsonl");
    const provider = writeHardeningProvider(root, logPath);
    const strictCfg = writeConfig(root, provider, "strict", {}, true);

    try {
      await withService(strictCfg, async (sock) => {
        const source = LanguageSource.create({
          endpoint: sock,
          target: "app",
          availability: "required",
          replaceInput: true,
          replaceResult: true,
          withholdResult: true,
        });
        await source.connect();
        try {
          const session = source.session({ sessionId: "js-iso-1" });
          try {
            // 1) Async generator reusing one mutable object + mutating in finally.
            const shared = { v: -1 };
            async function* reusingStream() {
              try {
                for (let i = 0; i < 3; i++) {
                  shared.v = i;
                  yield shared;
                }
              } finally {
                shared.v = 999;
              }
            }
            const wrappedStream = session.wrapToolStream(reusingStream, {
              name: "reusing",
              bufferStream: true,
            });
            const seen = [];
            for await (const item of wrappedStream({ command: "ok" })) seen.push(item);
            assert.deepEqual(seen, [{ v: 0 }, { v: 1 }, { v: 2 }]);

            // 2) Normal tool returning a mutable object mutated afterward.
            let ref;
            const wrappedMut = session.wrapTool(
              async () => {
                ref = { text: "clean", nested: [1, 2] };
                return ref;
              },
              { name: "mut" },
            );
            const delivered = await wrappedMut({ command: "ok" });
            ref.text = "TAMPERED";
            ref.nested.push(99);
            assert.deepEqual(delivered, { text: "clean", nested: [1, 2] });

            // 3) Dynamic lazy iterator / sync generator: rejected unbuffered, buffered into isolated copies.
            let syncStarted = 0;
            function* syncGen() {
              syncStarted += 1;
              yield "part";
            }
            await assert.rejects(
              () => session.wrapTool(syncGen, { name: "sgen" })({ command: "ok" }),
              { code: "source_stream_withhold_requires_buffering" },
            );
            assert.equal(syncStarted, 0);

            let iterClosed = 0;
            const createdIters = [];
            function returnsLazy() {
              const g = (function* () {
                const box = { n: 0 };
                try {
                  for (const i of [1, 2]) {
                    box.n = i;
                    yield box;
                  }
                } finally {
                  box.n = 999;
                  iterClosed += 1;
                }
              })();
              createdIters.push(g);
              return g;
            }
            await assert.rejects(
              () => session.wrapTool(returnsLazy, { name: "lazy" })({ command: "ok" }),
              (err) => err?.code === "source_stream_withhold_requires_buffering" && err.toolExecuted === true,
            );
            assert.equal(createdIters.at(-1).next().done, true);

            const bufferedIter = await session.wrapTool(returnsLazy, {
              name: "lazy_buf",
              bufferStream: true,
            })({ command: "ok" });
            assert.deepEqual([...bufferedIter], [{ n: 1 }, { n: 2 }]);
            assert.equal(iterClosed, 1);

            // 4) P1-3: Cumulative byte budget (exact boundary, single-chunk, multi-chunk).
            let closedCount = 0;
            async function* sizedStream(args) {
              try {
                for (const c of args.chunks) yield c;
              } finally {
                closedCount += 1;
              }
            }
            // JSON `"abcd"` is 6 bytes; 2 chunks = 12 bytes exact boundary.
            const bounded = session.wrapToolStream(sizedStream, {
              name: "sized",
              bufferStream: true,
              maxStreamBytes: 12,
            });
            const exact = [];
            for await (const c of bounded({ chunks: ["abcd", "efgh"] })) exact.push(c);
            assert.deepEqual(exact, ["abcd", "efgh"]);

            const recvSingle = [];
            const c0 = closedCount;
            await assert.rejects(
              async () => {
                for await (const c of bounded({ chunks: ["12345678901"] })) recvSingle.push(c);
              },
              (err) => err?.code === "source_stream_result_too_large" && err.toolExecuted === true,
            );
            assert.deepEqual(recvSingle, []);
            assert.equal(closedCount, c0 + 1);

            const recvMulti = [];
            const c1 = closedCount;
            await assert.rejects(
              async () => {
                for await (const c of bounded({ chunks: ["abcd", "efgh", "x"] })) recvMulti.push(c);
              },
              (err) => err?.code === "source_stream_result_too_large" && err.toolExecuted === true,
            );
            assert.deepEqual(recvMulti, []);
            assert.equal(closedCount, c1 + 1);
          } finally {
            await session.close();
          }
        } finally {
          await source.close();
        }
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);

test(
  "P0-4, P0-5 & P1-2: Canonical URL allowlist, scoped delegation, reinstall on cached ESM, default export & async generator",
  { skip: !AW_BINARY },
  async () => {
    const root = mkdtempSync(path.join(tmpdir(), "aw-js-mod-"));
    const logPath = path.join(root, "audit.jsonl");
    const provider = writeHardeningProvider(root, logPath);
    const cfg1 = writeConfig(root, provider, "p1", { replace_tag: "pol1" }, true);
    const cfg2 = writeConfig(root, provider, "p2", { replace_tag: "pol2" }, true);

    const dirA = path.join(root, "dirA");
    const dirB = path.join(root, "dirB");
    mkdirSync(dirA);
    mkdirSync(dirB);
    writeFileSync(
      path.join(dirA, "shared.mjs"),
      `export const UNTOUCHED = "A";
export default function defaultExport() { return "default-A"; }
export async function runTool(args) { return "rawA:" + args.command; }
export async function* streamTool(args) { yield "s:" + args.command; }
`,
      "utf8",
    );
    writeFileSync(
      path.join(dirB, "shared.mjs"),
      `export async function runTool(args) { return "rawB:" + args.command; }\n`,
      "utf8",
    );
    writeFileSync(
      path.join(dirA, "callerA.mjs"),
      `import def, { UNTOUCHED, runTool, streamTool } from "./shared.mjs";
export { def, UNTOUCHED, runTool, streamTool };
`,
      "utf8",
    );
    writeFileSync(
      path.join(dirB, "callerB.mjs"),
      `import { runTool } from "./shared.mjs";
export { runTool };
`,
      "utf8",
    );
    writeFileSync(
      path.join(root, "invalid_exports.mjs"),
      `export const NOT_A_FN = 42;
export function syncFn(args) { return "sync:" + args.command; }
`,
      "utf8",
    );
    writeFileSync(
      path.join(root, "query_mod.mjs"),
      `export async function qRun(args) { return "q:" + args.command; }\n`,
      "utf8",
    );

    try {
      await withService(cfg1, async (sock1) => {
        await withService(cfg2, async (sock2) => {
          // 1) P0-4 + P1-2: Relative "./shared.mjs" anchored to dirA ONLY wraps dirA/shared.mjs,
          //    preserves default export & UNTOUCHED & async generator protocol.
          const qUrl = `${pathToFileURL(path.join(root, "query_mod.mjs")).href}?v=1`;
          const handle1 = installAutoInstrumentation(
            {
              endpoint: sock1,
              target: "app",
              availability: "required",
              replaceInput: true,
              replaceResult: true,
              withholdResult: true,
              baseDir: dirA,
              targets: [
                { kind: "module", module: "./shared.mjs", exportName: "runTool", name: "runTool" },
                {
                  kind: "module",
                  module: "./shared.mjs",
                  exportName: "streamTool",
                  name: "streamTool",
                  bufferStream: true,
                },
                { kind: "module", module: qUrl, exportName: "qRun", name: "qRun" },
              ],
            },
          );

          const modA = await import(pathToFileURL(path.join(dirA, "callerA.mjs")).href);
          const modB = await import(pathToFileURL(path.join(dirB, "callerB.mjs")).href);

          // dirA is intercepted (pol1), dirB with identical relative specifier is untouched!
          assert.equal(await modA.runTool({ command: "REWRITE" }), "pol1:rawA:safe");
          assert.equal(await modB.runTool({ command: "BLOCK" }), "rawB:BLOCK");

          // Default export & untouched named export preserved (P1-2).
          assert.equal(modA.UNTOUCHED, "A");
          assert.equal(modA.def(), "default-A");

          // Async generator preserves direct async-iterable return (NOT wrapped in Promise!).
          const gen = modA.streamTool({ command: "REWRITE" });
          assert.equal(typeof gen[Symbol.asyncIterator], "function");
          const chunks = [];
          for await (const c of gen) chunks.push(c);
          assert.deepEqual(chunks, ["s:safe"]);

          // Target with normal query parameter ?v=1 works without corruption (P0-4).
          const qMod = await import(qUrl);
          assert.equal(await qMod.qRun({ command: "REWRITE" }), "pol1:q:safe");

          // Forged internal delegation scheme or ?aw_original=1 cannot bypass proxy.
          await assert.rejects(
            () => import("aw-instrument-orig:forged/" + encodeURIComponent(qUrl)),
            { code: "source_auto_delegation_denied" },
          );
          const forgedQueryMod = await import(
            `${pathToFileURL(path.join(dirA, "shared.mjs")).href}?aw_original=1`
          );
          assert.notEqual(forgedQueryMod, modA);

          // 2) P0-5: Uninstall -> raw passthrough -> reinstall with pol2 -> cached modA uses pol2!
          await handle1.uninstall();
          assert.equal(await modA.runTool({ command: "BLOCK" }), "rawA:BLOCK");

          const handle2 = installAutoInstrumentation({
            endpoint: sock2,
            target: "app",
            availability: "required",
            replaceInput: true,
            replaceResult: true,
            withholdResult: true,
            baseDir: dirA,
            targets: [
              { kind: "module", module: "./shared.mjs", exportName: "runTool", name: "runTool" },
            ],
          });
          try {
            // Existing cached binding from import #1 MUST execute policy 2!
            assert.equal(await modA.runTool({ command: "REWRITE" }), "pol2:rawA:safe");
            await assert.rejects(() => modA.runTool({ command: "BLOCK" }), SourceDenied);
            // Fresh dynamic import also executes policy 2!
            const modA2 = await import(pathToFileURL(path.join(dirA, "shared.mjs")).href);
            assert.equal(await modA2.runTool({ command: "REWRITE" }), "pol2:rawA:safe");
          } finally {
            await handle2.uninstall();
          }

          // 3) P1-2: Missing, non-function, and sync exports fail explicitly on load.
          const invPath = path.join(root, "invalid_exports.mjs");
          for (const [exp, code, q] of [
            ["missingFn", "source_auto_target_missing", "?case=missing"],
            ["NOT_A_FN", "source_auto_target_unsupported", "?case=nonfn"],
            ["syncFn", "source_auto_target_sync_unsupported", "?case=sync"],
          ]) {
            const url = `${pathToFileURL(invPath).href}${q}`;
            const h = installAutoInstrumentation({
              endpoint: sock1,
              target: "app",
              availability: "required",
              targets: [{ kind: "module", module: url, exportName: exp }],
            });
            try {
              await assert.rejects(() => import(url), { code });
            } finally {
              await h.uninstall();
            }
          }
        });
      });
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  },
);
