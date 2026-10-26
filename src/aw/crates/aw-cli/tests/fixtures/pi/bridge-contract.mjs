/** Callback boundary tests use the real bridge and a bounded command fixture, without AW. */
import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { installSessionBridge } from "../../../../aw-adapter/native/pi/bridge.mjs";

const output = fileURLToPath(
  new URL("../../../../../target/pi-bridge-contract/", import.meta.url),
);
mkdirSync(output, { recursive: true });

function fixture(
  t,
  {
    authorized = true,
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
        on_error: "report",
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
  const agent = {
    beforeToolCall: async () => {
      beforeCalls++;
    },
    afterToolCall: async () => {
      afterCalls++;
      return original;
    },
    subscribe: () => () => {},
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
