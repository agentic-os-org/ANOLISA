import test from "node:test";
import assert from "node:assert/strict";
import { qoderCapabilities, qoderInstallRoutes, qoderOptions } from "../src/index.mjs";

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
  routes: { hooks: [hook], events: {} },
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
  task.routes = postRoutes();
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


test("native Qoder session starts are observed without changing native Hooks", async () => {
  assert.equal(qoderCapabilities(false).events["session.start"], undefined);
  assert.deepEqual(qoderCapabilities(false, true).events["session.start"], ["observe"]);
  const calls = [];
  const task = scope(async (selected, event) => {
    calls.push([selected.event, event.session_id]);
    return { code: 0, stdout: Buffer.alloc(0) };
  });
  task.routes.hooks = [{ ...hook, event: "session.start", on_error: "report" }];
  let preparations = 0;
  task.prepare = () => { preparations++; };
  const attached = qoderOptions(task);
  const run = attached.options.hooks.SessionStart[0].hooks[0];
  await run({ hook_event_name: "SessionStart", session_id: "session", source: "compact" });
  assert.deepEqual(calls, []);
  assert.equal(preparations, 0);
  assert.deepEqual(await run({ hook_event_name: "SessionStart", session_id: "session", source: "startup" }), {});
  assert.deepEqual(calls, [["session.start", "session"]]);
  assert.equal(preparations, 1);
});

const post = {
  ...payload,
  hook_event_name: "PostToolUse",
  tool_response: "SYNTHETIC_RAW_SECRET",
};
const failedPost = {
  ...payload,
  hook_event_name: "PostToolUseFailure",
  error: "synthetic failure",
};
const replacement = {
  hookSpecificOutput: {
    hookEventName: "PostToolUse",
    updatedToolOutput: "REVIEWED_SAFE_BODY",
  },
};
function postRoutes() {
  const observe = { ...hook, event: "tool.after", step: "observe", on_error: "report" };
  return {
    hooks: [observe, { ...observe, step: "replace", replace_result: true }],
    events: {
      "tool.after": {
        steps: { observe: "report", replace: "report" },
        replacement_step: "replace",
      },
    },
  };
}

test("public Qoder install only confirms the declared final Post replacement", () => {
  assert.deepEqual(qoderCapabilities(false).events["tool.after"], ["observe", "replace_result"]);
  assert.equal(qoderInstallRoutes(postRoutes(), false), true);
  for (const mutate of [
    ({ hooks }) => hooks.reverse(),
    ({ hooks }) => { hooks[0].replace_result = true; },
    ({ hooks }) => { hooks[1].replace_result = "true"; },
    ({ hooks }) => { hooks[1].replace_result = false; },
    ({ hooks }) => { hooks[1].step = "observe"; },
    ({ hooks }) => { hooks[1].on_error = "withhold_result"; },
    ({ hooks }) => hooks.push({ ...hook, replace_result: true }),
    ({ hooks }) => hooks.push({ ...hook, replace_input: true }),
    ({ events }) => { events["tool.after"].replacement_step = "observe"; },
    ({ events }) => { delete events["tool.after"]; },
    ({ events }) => { events["tool.before"] = { replacement_step: "forged" }; },
  ]) {
    const routes = postRoutes();
    mutate(routes);
    assert.equal(qoderInstallRoutes(routes, false), false);
    assert.throws(() => qoderOptions({ routes }), /invalid_qoder_sdk_routes/);
  }
});

test("unexpected replacement from an observe step or a failed tool is refused", async () => {
  for (const [name, input, forgedAt] of [
    ["PostToolUse", post, "observe"],
    ["PostToolUseFailure", failedPost, "replace"],
  ]) {
    const task = scope(async (selected) => ({
      code: 0,
      stdout: Buffer.from(JSON.stringify(selected.step === forgedAt ? replacement : {})),
    }));
    task.routes = postRoutes();
    const attached = qoderOptions(task);
    const result = await attached.options.hooks[name][0].hooks[0](input, "call");
    assert.deepEqual(result, { continue: false, stopReason: "AW result check failed" });
    assert.equal(attached.failure.message, "unsupported_qoder_sdk_directive");
  }
});
