/** MCP composition uses the actual SDK parser; transport and AW service scope are fixtures. */
import test from "node:test";
import assert from "node:assert/strict";
import { managedMcpServer } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";
import { PRIVATE_EXECUTION_VERSION, executionEnvironment, hookEnvironment }
  from "../../../../aw-adapter/native/qoder-sdk/private.mjs";
import { sdk, z, pause, deferred, fixture } from "./parser-support.mjs";

const pre = { event: "tool.before", step: "required", on_error: "block", budget_ms: 100 };
const post = { event: "tool.after", step: "required", on_error: "report", budget_ms: 100 };
const lease = (extra = {}) => ({ version: PRIVATE_EXECUTION_VERSION,
  environment: {}, hookEnvironment: {}, secrets: {}, redact: value => value,
  release: async () => {}, ...extra });
const call = (f, id, args = {}) => f.transport.emit({ type: "control_request", request_id: id,
  request: { subtype: "mcp_message", server_name: "fixture",
    message: { jsonrpc: "2.0", id: 1, method: "tools/call", params: { name: "run", arguments: args } } } });
const server = (handler, inputSchema = {}) => managedMcpServer(sdk, { name: "fixture",
  tools: [{ name: "run", description: "Read fixture", inputSchema, handler }] });

async function until(predicate, message) {
  for (let attempt = 0; attempt < 1000; attempt++) {
    if (predicate()) return;
    await pause();
  }
  assert.fail(message);
}

test("Resident.create binds private MCP consumption and redacts before Post", { timeout: 5000 }, async t => {
  const order = [], policies = [];
  const secret = "fixture-mcp-credential";
  let approved, consumed, environment;
  const nativeServer = server(async (input, { signal }) => {
    assert.equal(signal.aborted, false);
    assert.equal(input, approved.input);
    assert(Object.isFrozen(input.payload));
    environment = executionEnvironment();
    consumed = environment.AW_TEST_MCP_SECRET;
    assert.deepEqual(hookEnvironment(), { AW_TEST_MCP_VIEW: "safe" });
    assert.notEqual(process.env.AW_TEST_MCP_SECRET, secret);
    order.push("handler");
    try {
      return { content: [{ type: "text", text: consumed },
        { type: "image", data: "cGl4ZWw=", mimeType: "image/png" }],
      structuredContent: { credential: consumed, command: input.payload.command }, isError: true };
    } finally { order.push("raw-finally"); }
  }, { payload: z.object({ command: z.string() }) });
  const factory = async context => {
    approved = context;
    assert(Object.isFrozen(context));
    assert(Object.isFrozen(context.input.payload));
    order.push("factory");
    return lease({ environment: { AW_TEST_MCP_VIEW: "safe" },
      hookEnvironment: { AW_TEST_MCP_VIEW: "safe" }, secrets: { AW_TEST_MCP_SECRET: secret },
      redact(value) {
        order.push("redact");
        return JSON.parse(JSON.stringify(value).replaceAll(secret, "[redacted]"));
      }, release: async () => { order.push("release"); } });
  };
  const f = await fixture(t, undefined, { mcpServers: { fixture: nativeServer },
    awExecutionFactory: factory }, [pre, post]);
  assert.equal(Object.hasOwn(f.composedOptions, "awExecutionFactory"), false);
  assert.equal(Object.hasOwn(f.transport.initialization, "awExecutionFactory"), false);
  f.scope.invoke = async (hook, payload) => {
    policies.push(structuredClone(payload));
    order.push(hook.event);
    if (hook.event === "tool.after") {
      assert(!JSON.stringify(payload).includes(secret));
      assert.equal(payload.tool_response.result.structuredContent.credential, "[redacted]");
    }
    return { code: 0, stdout: Buffer.from("{}") };
  };
  call(f, "private", { payload: { command: "approved" } });
  const result = (await f.response("private")).response.mcp_response.result;
  assert.equal(consumed, secret);
  assert.deepEqual(result, { content: [{ type: "text", text: "[redacted]" },
    { type: "image", data: "cGl4ZWw=", mimeType: "image/png" }],
  structuredContent: { credential: "[redacted]", command: "approved" }, isError: true });
  assert.deepEqual(order, ["tool.before", "factory", "handler", "raw-finally", "redact", "release", "tool.after"]);
  assert.equal(approved.invocationId, policies[0].tool_use_id);
  assert.equal(approved.toolName, "mcp__fixture__run");
  assert.equal(policies.length, 2);
  assert.equal(f.scope.invocations.size, 0);
  assert.throws(executionEnvironment, /no active tool environment/);
  assert.equal(environment.AW_TEST_MCP_SECRET, secret, "the consumer owns its copied environment");
});

test("private MCP cancellation joins raw finally and lease release before scope retirement", { timeout: 5000 }, async t => {
  const entered = deferred(), finish = deferred(), finalizing = deferred(), finalized = deferred();
  const releasing = deferred(), released = deferred();
  const order = [];
  let signal, closed = false, closing;
  const nativeServer = server(async (_input, options) => {
    signal = options.signal;
    entered.resolve();
    try { await finish.promise; return { content: [] }; }
    finally {
      finalizing.resolve();
      await finalized.promise;
      order.push("raw-finally");
    }
  });
  const f = await fixture(t, undefined, { mcpServers: { fixture: nativeServer },
    awExecutionFactory: async () => lease({ release: async () => {
      releasing.resolve();
      await released.promise;
      order.push("release");
    } }) }, []);
  const finishScope = f.scope.finish, closeScope = f.scope.close;
  f.scope.finish = async id => { order.push("scope-finish"); await finishScope(id); };
  f.scope.close = async () => { order.push("scope-close"); await closeScope(); };
  call(f, "cancel-private");
  try {
    await entered.promise;
    f.transport.emit({ type: "control_cancel_request", request_id: "cancel-private" });
    await until(() => signal.aborted, "native cancellation must reach the explicit handler");
    closing = f.query.close().then(() => { closed = true; });
    finish.resolve();
    await finalizing.promise;
    assert.equal(closed, false);
    assert.equal(f.scope.invocations.size, 1);
    assert.deepEqual(order, []);
    finalized.resolve();
    await releasing.promise;
    assert.deepEqual(order, ["raw-finally"]);
    assert.equal(closed, false);
    assert.equal(f.scope.invocations.size, 1);
    released.resolve();
    await closing;
    assert.deepEqual(order, ["raw-finally", "release", "scope-finish", "scope-close"]);
    assert.equal(f.transport.responses.has("cancel-private"), false);
    assert.equal(f.scope.invocations.size, 0);
    assert.equal(f.scope.closed, true);
  } finally {
    finish.resolve();
    finalized.resolve();
    released.resolve();
    await closing;
  }
});

test("strict MCP Post withholds the parser response until native shutdown", { timeout: 5000 }, async t => {
  const shuttingDown = deferred(), shutdown = deferred();
  const order = [];
  let calls = 0;
  const f = await fixture(t, undefined, { mcpServers: { fixture: server(async () => {
    calls++;
    return { content: [{ type: "text", text: "withheld fixture result" }] };
  }) } }, [{ ...post, on_error: "withhold_result" }]);
  f.scope.invoke = async () => ({ code: 1, stdout: Buffer.from("") });
  const nativeClose = f.resident.nativeClose, finishScope = f.scope.finish;
  // Hold the real parser's close call; this fixture does not start a Worker.
  f.resident.nativeClose = async () => {
    order.push("shutdown-start");
    shuttingDown.resolve();
    await shutdown.promise;
    await nativeClose();
    order.push("shutdown-finish");
  };
  f.scope.finish = async id => { order.push("scope-finish"); await finishScope(id); };
  call(f, "strict-post");
  try {
    await shuttingDown.promise;
    assert.equal(calls, 1);
    assert(f.resident.failure);
    assert.equal(f.transport.responses.has("strict-post"), false);
    assert.equal(f.scope.invocations.size, 1);
    assert.throws(() => f.scope.checkExecution(), /closed|unavailable/);
    assert.deepEqual(order, ["shutdown-start"]);
    shutdown.resolve();
    await f.query.close();
    assert.deepEqual(order, ["shutdown-start", "shutdown-finish", "scope-finish"]);
    assert.equal(f.transport.closed, true);
    assert.equal(f.transport.responses.has("strict-post"), false);
    assert(!JSON.stringify([...f.transport.responses]).includes("withheld fixture result"));
    assert.equal(f.scope.invocations.size, 0);
  } finally { shutdown.resolve(); }
});

test("private MCP factory suspension preserves approved parsed arguments", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  let parsed, seen, consumed, approved;
  const nativeServer = server(async input => {
    seen = input.payload.command;
    consumed = input;
    return { content: [] };
  }, { payload: z.object({ command: z.string() }).transform(value => { parsed = value; return value; }) });
  const f = await fixture(t, undefined, { mcpServers: { fixture: nativeServer },
    awExecutionFactory: async context => {
      approved = context;
      entered.resolve();
      await release.promise;
      return lease();
    } }, []);
  call(f, "private-snapshot", { payload: { command: "approved" } });
  try {
    await entered.promise;
    parsed.command = "changed-after-authorization";
    release.resolve();
    const response = await f.response("private-snapshot");
    assert.equal(response.response.mcp_response.result.isError, undefined);
    assert.equal(seen, "approved");
    assert.equal(consumed, approved.input);
    assert(Object.isFrozen(consumed.payload));
  } finally { release.resolve(); }
});

test("MCP tracking suspension preserves approved parsed arguments without a private factory", { timeout: 5000 }, async t => {
  let parsed, seen, changed = false;
  const nativeServer = server(async input => {
    seen = input.payload.command;
    return { content: [] };
  }, { payload: z.object({ command: z.string() }).transform(value => { parsed = value; return value; }) });
  const f = await fixture(t, undefined, { mcpServers: { fixture: nativeServer } }, []);
  const track = f.resident.track.bind(f.resident);
  f.resident.track = (callback, entry) => {
    const result = track(callback, entry);
    if (parsed && entry?.pending && !changed) {
      // The original tracker schedules the handler in its next microtask.
      changed = true;
      parsed.command = "changed-after-authorization";
    }
    return result;
  };
  call(f, "ordinary-snapshot", { payload: { command: "approved" } });
  const response = await f.response("ordinary-snapshot");
  assert.equal(response.response.mcp_response.result.isError, undefined);
  assert.equal(changed, true);
  assert.equal(seen, "approved");
});
