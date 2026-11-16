import test from "node:test";
import assert from "node:assert/strict";
import { qoderOptions } from "../../../crates/aw-adapter/native/qoder-sdk/sdk.mjs";

const hook = {
  event: "tool.before",
  step: "policy",
  on_error: "block",
  budget_ms: 100,
};
const payload = {
  hook_event_name: "PreToolUse",
  session_id: "session",
  tool_use_id: "call",
  tool_name: "Bash",
  tool_input: { command: "true" },
};
const native = () => ({
  hooks: { PreToolUse: [{ matcher: "Bash", hooks: [async () => ({})] }] },
  model: "native-model",
});
const scope = (invoke) => ({
  fallback: false,
  routes: { hooks: [hook] },
  invoke,
  abortEvent: async () => {},
});
const callback = (attached) =>
  attached.options.hooks.PreToolUse.at(-1).hooks[0];

test("native hooks/options survive composition and AW denial remains explicit", async () => {
  const options = native();
  const attached = qoderOptions(
    scope(async () => ({ code: 2, stdout: Buffer.from('"denied"') })),
    options,
  );
  assert.equal(attached.options.model, options.model);
  assert.equal(options.hooks.PreToolUse.length, 1);
  assert.equal(attached.options.hooks.PreToolUse.length, 2);
  assert.equal(
    attached.options.hooks.PreToolUse[0].hooks[0],
    options.hooks.PreToolUse[0].hooks[0],
  );
  const result = await callback(attached)(payload, "call");
  assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
  assert.throws(
    () =>
      qoderOptions(
        scope(() => {}),
        attached.options,
      ),
    /already installed/,
  );
});

test("Provider and native identity failures deny, while lost transport stays neutral", async () => {
  const task = scope(async () => {
    throw Error("sdk_step_failed");
  });
  const attached = qoderOptions(task, native());
  assert.equal(
    (await callback(attached)(payload, "call")).hookSpecificOutput
      .permissionDecision,
    "deny",
  );
  assert.equal(attached.failure.message, "sdk_step_failed");
  const mismatch = qoderOptions(
    scope(() => {
      throw Error("must not call");
    }),
    native(),
  );
  assert.equal(
    (await callback(mismatch)(payload, "other")).hookSpecificOutput
      .permissionDecision,
    "deny",
  );
  assert.equal(mismatch.failure.message, "native_tool_identity_mismatch");
  task.fallback = true;
  assert.deepEqual(await callback(attached)(payload, "call"), {});
  const abort = new AbortController();
  abort.abort();
  await assert.rejects(
    callback(attached)(payload, "call", { signal: abort.signal }),
  );
});

test("post-tool Provider failure stops the Query and does not become fallback", async () => {
  const task = scope(async () => {
    throw Error("sdk_step_failed");
  });
  task.routes.hooks = [{ ...hook, event: "tool.after" }];
  let interrupts = 0;
  const attached = qoderOptions(task);
  attached.bind({
    interrupt: async () => {
      interrupts++;
    },
  });
  const run = attached.options.hooks.PostToolUse[0].hooks[0];
  const result = await run(
    { ...payload, hook_event_name: "PostToolUse", tool_response: "ok" },
    "call",
  );
  assert.equal(result.continue, false);
  assert.equal(interrupts, 1);
  assert.equal(task.fallback, false);
});

test("native response errors drain the partial event before reporting denial", async () => {
  const order = [];
  const task = scope(async () => {
    order.push("invoke");
    return { code: 0, stdout: Buffer.from('{"unsupported":true}') };
  });
  task.routes.hooks.push({ ...hook, step: "later" });
  task.abortEvent = async (route, native) => {
    assert.equal(route.event, "tool.before");
    assert.equal(native.tool_use_id, "call");
    order.push("drained");
  };
  const attached = qoderOptions(task);
  const result = await callback(attached)(payload, "call");
  assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
  assert.equal(attached.failure.message, "unsupported_qoder_sdk_directive");
  assert.deepEqual(order, ["invoke", "drained"]);
});
