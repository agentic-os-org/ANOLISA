/** Callback boundary tests use the real bridge and a bounded command fixture, without AW. */
import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { installSessionBridge } from "../../../../aw-adapter/native/pi/bridge.mjs";
import { Scope } from "../../../../aw-adapter/native/shared/scope.mjs";

const output = fileURLToPath(
  new URL("../../../../../target/pi-bridge-contract/", import.meta.url),
);
mkdirSync(output, { recursive: true });

function fixture(
  t,
  {
    authorized = true,
    strict = false,
    mode = "replace",
    content = [{ type: "text", text: "native" }],
  } = {},
) {
  const directory = mkdtempSync(join(output, "case-"));
  const helper = join(directory, "callback");
  writeFileSync(
    helper,
    `#!/usr/bin/env node
let input='';for await (const chunk of process.stdin) input+=chunk;
const payload=JSON.parse(input);const result=structuredClone(payload.tool_response);
if(result.content.length === 1 && result.content[0].type === 'text') result.content[0].text='compact';
const mode=${JSON.stringify(mode)};
if(mode==='withhold') { console.log('{"aw_withheld":true}'); process.exit(0); }
if(mode==='invalid') { console.log('invalid'); process.exit(0); }
if(mode==='exit') process.exit(1);
if(mode==='clean') { console.log('{}'); process.exit(0); }
if(mode==='metadata') result.isError=!result.isError;
if(mode==='empty') result.content[0].text='';
if(mode==='number') result.content[0].text=7;
console.log(JSON.stringify({aw_replacement:result}));
`,
    { mode: 0o700 },
  );
  const config = {
    executable: helper,
    binding: join(directory, "unused-binding"),
    ready_path: join(directory, "unused-ready"),
    ready_token: "fixture",
    replacement_step: authorized ? "replace" : null,
    hooks: [
      {
        event: "tool.after",
        step: "replace",
        on_error: strict ? "withhold_result" : "report",
        budget_ms: 1000,
      },
    ],
  };
  const path = join(directory, "config.json");
  writeFileSync(path, JSON.stringify(config), { mode: 0o600 });
  process.env.AW_NATIVE_CONFIG = path;
  let beforeCalls = 0,
    afterCalls = 0;
  const original = {
    content,
    details: { native: true },
    usage: { input: 0, output: 0 },
    isError: true,
    terminate: false,
  };
  const events = [];
  const agent = {
    beforeToolCall: async () => {
      beforeCalls++;
    },
    afterToolCall: async () => {
      afterCalls++;
      return original;
    },
    subscribe: () => () => {},
    processEvents: async event => events.push(event),
  };
  const originalBefore = agent.beforeToolCall;
  const originalAfter = agent.afterToolCall;
  const installed = installSessionBridge({
    agent,
    sessionManager: { getSessionId: () => "durable-session" },
  });
  t.after(() => {
    installed.dispose();
    rmSync(directory, { recursive: true, force: true });
  });
  const context = {
    toolCall: { id: "reused-call", name: "test" },
    args: {},
    result: { content: [{ type: "text", text: "executed" }], details: {} },
    isError: false,
  };
  return {
    agent,
    events,
    context,
    original,
    installed,
    originalBefore,
    originalAfter,
    counts: () => ({ beforeCalls, afterCalls }),
  };
}

test("authorized text replacement calls native callbacks once and preserves overrides", async (t) => {
  const f = fixture(t);
  await f.agent.beforeToolCall(f.context);
  const result = await f.agent.afterToolCall(f.context);
  assert.equal(result.content[0].text, "compact");
  assert.strictEqual(result.details, f.original.details);
  assert.strictEqual(result.usage, f.original.usage);
  assert.equal(result.isError, true);
  assert.equal(result.terminate, false);
  assert.deepEqual(f.counts(), { beforeCalls: 1, afterCalls: 1 });
});

for (const mode of ["clean", "replace"])
  test(`strict ${mode} delivers detached metadata and the full checked envelope`, async (t) => {
    const f = fixture(t, { strict: true, mode });
    f.original.details = null;
    f.context.result.details = null;
    f.context.result.extra = { value: "checked" };
    await f.agent.beforeToolCall(f.context);
    const override = await f.agent.afterToolCall(f.context);
    // These aliases can survive beyond the callback, including invocation cleanup.
    f.original.content[0].text = "unchecked";
    f.original.usage.input = 99;
    f.context.result.details = { value: "unchecked" };
    f.context.result.extra.value = "unchecked";
    f.context.result.addedToolNames = ["unchecked"];
    // Pi 0.84.2 adopts supported overrides and retains execute()'s extra fields.
    const delivered = { ...f.context.result };
    for (const key of ["content", "details", "usage", "terminate"])
      delivered[key] = override[key] ?? delivered[key];
    await f.agent.processEvents({ type: "tool_execution_end", toolCallId: f.context.toolCall.id,
      result: delivered, isError: override.isError });
    assert.equal(delivered.content[0].text, mode === "clean" ? "native" : "compact");
    assert.equal(delivered.details, null);
    assert.deepEqual(delivered.extra, { value: "checked" });
    assert.deepEqual(delivered.usage, { input: 0, output: 0 });
    assert.equal(delivered.terminate, false);
    assert.equal(f.events[0].isError, true);
    assert(!JSON.stringify(f.events).includes("unchecked"));
    assert.equal(f.context.result.extra.value, "unchecked");
  });

test("strict snapshot errors do not expose the uncloneable value", async (t) => {
  const f = fixture(t, { strict: true, mode: "clean" });
  f.original.details = { callback: function PRIVATE_OUTPUT_SECRET() {} };
  await f.agent.beforeToolCall(f.context);
  await assert.rejects(f.agent.afterToolCall(f.context), {
    message: "AW could not snapshot tool result; result withheld by policy",
  });
});
for (const [name, options] of [
  ["unauthorized replacement", { authorized: false }],
  ["changed native metadata", { mode: "metadata" }],
  ["empty text", { mode: "empty" }],
  ["non-string text", { mode: "number" }],
  [
    "multiple blocks",
    {
      content: [
        { type: "text", text: "first" },
        { type: "text", text: "second" },
      ],
    },
  ],
  [
    "image block",
    { content: [{ type: "image", mimeType: "image/png", data: "fixture" }] },
  ],
])
  test(`${name} reports failure and preserves original result`, async (t) => {
    const f = fixture(t, options);
    await f.agent.beforeToolCall(f.context);
    const result = await f.agent.afterToolCall(f.context);
    assert.strictEqual(result, f.original);
    assert.deepEqual(f.counts(), { beforeCalls: 1, afterCalls: 1 });
  });

test("managed callbacks cannot be replaced and are restored on disposal", (t) => {
  const f = fixture(t);
  for (const key of ["beforeToolCall", "afterToolCall"]) {
    const callback = f.agent[key];
    assert.throws(() => {
      f.agent[key] = async () => undefined;
    }, /AW owns/);
    assert.equal(Reflect.deleteProperty(f.agent, key), false);
    assert.throws(
      () => Object.defineProperty(f.agent, key, { value: undefined }),
      TypeError,
    );
    assert.strictEqual(f.agent[key], callback);
  }
  f.installed.dispose();
  assert.strictEqual(f.agent.beforeToolCall, f.originalBefore);
  assert.strictEqual(f.agent.afterToolCall, f.originalAfter);
  const replacement = async () => undefined;
  f.agent.beforeToolCall = replacement;
  f.agent.afterToolCall = replacement;
  assert.strictEqual(f.agent.beforeToolCall, replacement);
  assert.strictEqual(f.agent.afterToolCall, replacement);
});

test("unconfirmed input retirement fences the next prompt after drain", async (t) => {
  const directory = mkdtempSync(join(output, "retirement-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  const config = { executable: "fixture", binding: "fixture", ready_path: "fixture",
    ready_token: "fixture", hooks: [{ event: "input.submit", step: "scan",
      on_error: "report", budget_ms: 1000 }] };
  process.env.AW_NATIVE_CONFIG = join(directory, "config.json");
  writeFileSync(process.env.AW_NATIVE_CONFIG, JSON.stringify(config));
  for (const mode of ["timeout", "spawn", "exit", "invalid"]) {
    const scope = new Scope(config);
    scope.handle = { fixture: true };
    scope.command = async (_action, _value, _timeout, hook) => {
      if (hook?.close_event) {
        if (mode === "timeout" || mode === "spawn") throw new Error(mode);
        return { code: mode === "exit" ? 1 : 0,
          stdout: Buffer.from(mode === "invalid" ? "unexpected" : "") };
      }
      return { code: 0, stdout: Buffer.alloc(0) };
    };
    scope.invoke = async () => ({ code: 0, stdout: Buffer.from("{}") });
    const bridge = installSessionBridge({ agent: { subscribe: () => () => {}, abort() {} },
      sessionManager: { getSessionId: () => "session" } }, scope);
    try {
      scope.begin();
      await assert.rejects(bridge.submit("input", "turn", new AbortController().signal));
      await bridge.drain();
      assert.equal(scope.failed, true, mode);
      assert.throws(() => scope.begin(), /AW/, mode);
      assert.equal(scope.submissionSequence, 1);
    } finally { bridge.dispose(); }
  }
});

test("strict after withholds audited faults and suppresses every intermediate update", async (t) => {
  const f = fixture(t, { strict: true, mode: "withhold" });
  await f.agent.beforeToolCall(f.context);
  await f.agent.processEvents({ type: "tool_execution_update", partialResult: "private" });
  assert.deepEqual(f.events, []);
  await assert.rejects(f.agent.afterToolCall(f.context), /result withheld by policy/);
  const event = { type: "tool_execution_end", result: "native error placeholder" };
  await f.agent.processEvents(event);
  assert.deepEqual(f.events, [event]);
  assert.throws(() => { f.agent.processEvents = () => {}; }, /AW owns/);
  f.installed.dispose();
  await f.agent.processEvents({ type: "tool_execution_update", partialResult: "native" });
  assert.equal(f.events.length, 2);
});

for (const mode of ["invalid", "exit", "metadata"])
  test(`strict ${mode} remains a callback failure`, async (t) => {
    const f = fixture(t, { strict: true, mode });
    await f.agent.beforeToolCall(f.context);
    await assert.rejects(f.agent.afterToolCall(f.context), /result withheld by policy/);
  });

test("report-only streams and final replacements retain native semantics", async (t) => {
  const f = fixture(t);
  const event = { type: "tool_execution_update", partialResult: "native" };
  await f.agent.processEvents(event);
  assert.strictEqual(f.events[0], event);
});

test("non-strict callbacks cannot consume an audited withholding marker", async (t) => {
  const f = fixture(t, { mode: "withhold" });
  await f.agent.beforeToolCall(f.context);
  assert.strictEqual(await f.agent.afterToolCall(f.context), f.original);
});
