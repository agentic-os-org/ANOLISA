/** Exercise the pinned SDK's real control parser without credentials or a model connection. */
import test from "node:test";
import assert from "node:assert/strict";
import { Resident } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";
import { managedMcpServer, admitMcp } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";
import { sdk, z, pause, deferred, fixture } from "./parser-support.mjs";
import "./parser-mcp.test.mjs";
import "./parser-child.test.mjs";
import "./parser-command-wrapper.test.mjs";
import "./parser-command-permission.test.mjs";

test("actual SDK parser binds model identity before native callback dispatch", { timeout: 5000 }, async t => {
  let calls = 0;
  const f = await fixture(t, async () => { calls++; return {}; });
  assert.equal(f.query.handleControlRequest, undefined);
  assert.equal(f.query.recordInboundSessionMessage, undefined);
  assert.equal(f.query.transport, undefined);
  assert(Object.isFrozen(f.query));
  f.model("native-1");
  f.hook("native-1", "request-1");
  assert.equal((await f.response("request-1")).subtype, "success");
  assert.equal(calls, 1);
  assert.equal(f.scope.invocations.size, 1);
  f.transport.holdBarrier = true;
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "native-1" }] } });
  for (let attempt = 0; !f.transport.barrier && attempt < 100; attempt++) await pause();
  assert(f.transport.barrier, "native terminal queue barrier must be requested");
  assert.equal(f.scope.invocations.size, 1);
  f.transport.holdBarrier = false;
  f.transport.write(JSON.stringify(f.transport.barrier));
  for (let attempt = 0; f.scope.invocations.size && attempt < 100; attempt++) await pause();
  assert.equal(f.scope.invocations.size, 0);
});

test("actual SDK cancellation does not release an original Hook finalizer", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  const f = await fixture(t, async () => {
    entered.resolve();
    try { await release.promise; } finally { f.finalized = true; }
    return {};
  });
  f.model("native-held");
  f.hook("native-held", "held");
  await entered.promise;
  f.transport.emit({ type: "control_cancel_request", request_id: "held" });
  await pause();
  let closed = false;
  const close = f.resident.close().then(() => { closed = true; });
  await pause();
  assert.equal(closed, false);
  assert.equal(f.scope.invocations.size, 1);
  release.resolve();
  await close;
  assert.equal(f.finalized, true);
  assert.equal(f.scope.invocations.size, 0);
});

test("unknown native callbacks deny before the application handler", { timeout: 5000 }, async t => {
  let calls = 0;
  const f = await fixture(t, async () => { calls++; return {}; });
  f.hook("unknown", "forged");
  const result = await f.response("forged");
  assert.equal(result.response.hookSpecificOutput.permissionDecision, "deny");
  assert.equal(calls, 0);
  assert.equal(f.scope.invocations.size, 0);
});

test("late model frames preserve the first native identity rejection", { timeout: 5000 }, async t => {
  let calls = 0;
  const f = await fixture(t, async () => { calls++; return {}; });
  const pending = f.query.next();
  const rejected = assert.rejects(pending, /unknown native tool identity/);
  f.hook("unknown-child", "forged-child");
  assert.equal((await f.response("forged-child")).response.hookSpecificOutput.permissionDecision, "deny");
  f.model("late-frame");
  await rejected;
  assert.equal(calls, 0);
  assert.equal(f.scope.closed, true);
});

test("model registration failure retains its cause and fences the Query", { timeout: 5000 }, async t => {
  const f = await fixture(t);
  const failure = new Error("registration fixture rejected");
  f.scope.register = async () => { throw failure; };
  f.model("registration-failed");
  for (let attempt = 0; !f.resident.failure && attempt < 100; attempt++) await pause();
  assert.equal(f.resident.failure, failure);
  await assert.rejects(f.query.next(), error => error === failure);
  assert.equal(f.scope.closed, true);
  assert.equal(f.scope.invocations.size, 0);
});

test("long native sequences retire active resources and reject late callbacks", { timeout: 15000 }, async t => {
  const f = await fixture(t);
  for (let sequence = 0; sequence < 1280; sequence++) {
    const id = `call-${sequence}`;
    f.model(id);
    f.hook(id, `request-${sequence}`);
    await f.response(`request-${sequence}`);
    f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: id }] } });
    for (let attempt = 0; f.scope.invocations.size && attempt < 100; attempt++) await pause();
    assert.equal(f.scope.invocations.size, 0);
    assert.equal(f.resident.identities.active.size, 0);
  }
  f.hook("call-0", "late");
  assert.equal((await f.response("late")).response.hookSpecificOutput.permissionDecision, "deny");
});

test("managed MCP retains schemas, annotations and structured error results", { timeout: 5000 }, async t => {
  let calls = 0;
  const result = { content: [{ type: "text", text: "native" }], structuredContent: { count: 3 }, isError: true };
  const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read",
    description: "Read fixture", inputSchema: {}, annotations: { readOnlyHint: true },
    permissionPolicy: "always_ask", handler: async () => { calls++; return result; } }] });
  const f = await fixture(t, undefined, { mcpServers: { fixture: server } });
  f.transport.emit({ type: "control_request", request_id: "mcp-list", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1, method: "tools/list", params: {} } } });
  const list = (await f.response("mcp-list")).response.mcp_response.result.tools[0];
  assert.equal(list.name, "read");
  assert.equal(list.annotations.readOnlyHint, true);
  assert.equal(list.inputSchema.type, "object");
  f.transport.emit({ type: "control_request", request_id: "mcp-call", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 2,
      method: "tools/call", params: { name: "read", arguments: {} } } } });
  assert.deepEqual((await f.response("mcp-call")).response.mcp_response.result, result);
  assert.equal(calls, 1);
  assert.equal(f.scope.invocations.size, 0);
  assert.throws(() => admitMcp({ mcpServers: { other: server } }), /fresh managed/);
  for (const schema of [z.string().default("unchecked"), z.string().transform(() => "unchecked")]) {
    let entered = false;
    const transformed = managedMcpServer(sdk, { name: "parsed", tools: [{ name: "run",
      description: "Parsed fixture", inputSchema: { command: schema },
      handler: async () => { entered = true; return { content: [] }; } }] });
    const parsed = await fixture(t, undefined, { mcpServers: { parsed: transformed } });
    const argumentsValue = schema.safeParse(undefined).success ? {} : { command: "approved" };
    parsed.transport.emit({ type: "control_request", request_id: "parsed", request: {
      subtype: "mcp_message", server_name: "parsed", message: { jsonrpc: "2.0", id: 3,
        method: "tools/call", params: { name: "run", arguments: argumentsValue } } } });
    const rejected = (await parsed.response("parsed")).response.mcp_response.result;
    assert.equal(rejected.isError, true);
    assert.match(rejected.content[0].text, /schema changed the approved/);
    assert.equal(entered, false);
  }
});

test("managed MCP Pre denial prevents actual handler execution", { timeout: 5000 }, async t => {
  let calls = 0;
  const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read",
    description: "Read fixture", inputSchema: {},
    handler: async () => { calls++; return { content: [] }; } }] });
  const f = await fixture(t, undefined, { mcpServers: { fixture: server } },
    [{ event: "tool.before", step: "required", on_error: "block", budget_ms: 100 }]);
  f.scope.invoke = async () => ({ code: 2, stdout: Buffer.from('"policy denied"') });
  f.transport.emit({ type: "control_request", request_id: "denied", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1,
      method: "tools/call", params: { name: "read", arguments: {} } } } });
  assert.equal((await f.response("denied")).subtype, "error");
  assert.equal(calls, 0);
  assert.equal(f.scope.invocations.size, 0);
});

test("native transport loss closes the scope and public output hides private partials", { timeout: 5000 }, async t => {
  const f = await fixture(t);
  f.transport.emit({ type: "stream_event", event: { type: "message_start" } });
  f.model("visible");
  const output = await f.query.next();
  assert.equal(output.value.type, "assistant");
  f.transport.failure = new Error("fixture transport lost");
  f.transport.changed.resolve();
  await assert.rejects(f.query.next(), /fixture transport lost/);
  assert.equal(f.scope.closed, true);
  assert.equal(f.scope.invocations.size, 0);
});

test("credential and model callback finalizers survive native abortable waits", { timeout: 5000 }, async t => {
  for (const subtype of ["fetch_job_token", "get_model_policy"]) {
    const entered = deferred(), release = deferred();
    let finalized = false;
    const callback = async () => {
      entered.resolve();
      try { await release.promise; } finally { finalized = true; }
      return subtype === "fetch_job_token" ? { token: "fixture" } : { model: "fixture" };
    };
    const extra = subtype === "fetch_job_token" ?
      { auth: { type: "jobToken", fetchJobToken: callback } } : { resolveModel: callback };
    const f = await fixture(t, undefined, extra);
    f.transport.emit({ type: "control_request", request_id: subtype, request: { subtype } });
    await entered.promise;
    f.transport.emit({ type: "control_cancel_request", request_id: subtype });
    await pause();
    let closed = false;
    const closing = f.query.close().then(() => { closed = true; });
    await pause();
    assert.equal(closed, false);
    release.resolve();
    await closing;
    assert(finalized);
    assert(f.scope.closed);
  }
});

const policyHooks = ["tool.before", "tool.after"].map(event =>
  ({ event, step: "required", on_error: event === "tool.before" ? "block" : "report", budget_ms: 100 }));

test("managed native MCP Hooks delegate each policy phase once through Resident.create", { timeout: 5000 }, async t => {
  const users = [], policies = [];
  let calls = 0;
  const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read",
    description: "Read fixture", inputSchema: {}, handler: async () => {
      calls++;
      return { content: [{ type: "text", text: "native result" }] };
    } }] });
  const f = await fixture(t, async (input, id) => {
    users.push([input.hook_event_name, id]);
    return {};
  }, { mcpServers: { fixture: server } }, policyHooks);
  f.scope.invoke = async (hook, payload) => {
    policies.push({ event: hook.event, payload });
    return { code: 0, stdout: Buffer.from("{}") };
  };
  const native = "model-mcp", name = "mcp__fixture__read";
  const mcpContext = { server_name: "fixture", tool_name: "read" };
  f.model(native, {}, name);
  for (const group of [0, 1]) {
    f.hook(native, `pre-${group}`, "PreToolUse", {}, { name, mcpContext, group });
    assert.equal((await f.response(`pre-${group}`)).subtype, "success");
  }
  assert.equal(policies.length, 0, "native policy Hooks must delegate to the managed MCP dispatch");
  f.transport.emit({ type: "control_request", request_id: "managed-call", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1,
      method: "tools/call", params: { name: "read", arguments: {} } } } });
  assert.equal((await f.response("managed-call")).response.mcp_response.result.content[0].text, "native result");
  for (const group of [0, 1]) {
    f.hook(native, `post-${group}`, "PostToolUse", {}, { name, mcpContext, group });
    assert.equal((await f.response(`post-${group}`)).subtype, "success");
  }
  assert.deepEqual(users, [["PreToolUse", native], ["PostToolUse", native]]);
  assert.deepEqual(policies.map(item => item.event), ["tool.before", "tool.after"]);
  assert.equal(calls, 1);
  const [before, after] = policies.map(item => item.payload);
  assert.equal(before.tool_use_id, after.tool_use_id);
  assert.notEqual(before.tool_use_id, f.resident.identities.active.get(native).id);
  assert.equal(before.aw_parent_query_id, f.resident.id);
  assert.equal(before.aw_native_request_id, "managed-call");
  assert.equal(before.aw_parent_tool_use_id, undefined);
  assert.equal(f.scope.invocations.size, 1, "the model invocation remains until its native terminal result");
});

test("unmanaged MCP server or tool context retains native policy checks", { timeout: 5000 }, async t => {
  for (const mcpContext of [{ server_name: "unknown", tool_name: "read" },
    { server_name: "fixture", tool_name: "unknown" }]) {
    const policies = [], users = [];
    const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read",
      description: "Read fixture", inputSchema: {}, handler: async () => ({ content: [] }) }] });
    const f = await fixture(t, async input => { users.push(input.hook_event_name); return {}; },
      { mcpServers: { fixture: server } }, policyHooks);
    f.scope.invoke = async hook => {
      policies.push(hook.event);
      return { code: 0, stdout: Buffer.from("{}") };
    };
    const name = "mcp__fixture__read";
    f.model("unmanaged-context", {}, name);
    for (const event of ["PreToolUse", "PostToolUse"]) {
      for (const group of [0, 1]) {
        const request = `${event}-${group}`;
        f.hook("unmanaged-context", request, event, {}, { name, mcpContext, group });
        assert.equal((await f.response(request)).subtype, "success");
      }
    }
    assert.deepEqual(policies, ["tool.before", "tool.after"]);
    assert.deepEqual(users, ["PreToolUse", "PostToolUse"]);
  }
});

const gateInput = { sessionId: "session", turnIndex: 1, cwd: process.cwd(),
  prompt: "fixture prompt", response: "fixture response" };
for (const kind of ["memory", "skill"]) {
  test(`${kind} gate finalizer survives native control cancellation`, { timeout: 5000 }, async t => {
    const entered = deferred(), release = deferred();
    let finalized = false, signal;
    const callback = async (input, options) => {
      assert.deepEqual(input, gateInput);
      signal = options.signal;
      entered.resolve();
      try { await release.promise; } finally { finalized = true; }
      return { run: false, reason: "fixture gate" };
    };
    const turnComplete = { timeoutMs: 321, onGateError: "report_failed",
      [kind === "memory" ? "shouldGenerate" : "shouldReview"]: callback };
    const extra = kind === "memory" ? { memory: { mode: "custom", generation: { turnComplete } } } :
      { evolution: { skill: { mode: "custom", generation: { turnComplete } } } };
    const f = await fixture(t, undefined, extra);
    const configured = kind === "memory" ? f.transport.initialization.memory : f.transport.initialization.evolution.skill;
    const gate = configured.generation.turnComplete;
    assert.equal(gate.timeoutMs, 321);
    assert.equal(gate.onGateError, "report_failed");
    assert.equal(turnComplete[kind === "memory" ? "shouldGenerate" : "shouldReview"], callback);
    const subtype = kind === "memory" ? "memory_should_generate" : "skill_evolution_should_review";
    const callbackId = kind === "memory" ? gate.shouldGenerateCallbackId : gate.shouldReviewCallbackId;
    assert.equal(typeof callbackId, "string");
    f.transport.emit({ type: "control_request", request_id: subtype,
      request: { subtype, callbackId, input: gateInput } });
    await entered.promise;
    let closed = false, closing;
    try {
      f.transport.emit({ type: "control_cancel_request", request_id: subtype });
      await pause();
      assert.equal(signal.aborted, true);
      assert.equal(f.resident.query.pendingInboundRequests.has(subtype), false);
      assert.equal(f.transport.responses.has(subtype), false, "native cancellation suppresses its response");
      closing = f.query.close().then(() => { closed = true; });
      await pause();
      assert.equal(closed, false);
      assert.equal(f.scope.closed, false);
    } finally {
      release.resolve();
      await closing;
    }
    assert.equal(finalized, true);
    assert.equal(f.scope.closed, true);
  });
}

for (const subtype of ["memory_generation", "memory_consumption", "skill_evolution"]) {
  test(`${subtype} result finalizer drains through the real SDK system parser`, { timeout: 5000 }, async t => {
    const entered = deferred(), release = deferred();
    let finalized = false;
    const result = subtype === "memory_consumption" ? { status: "success", files: [] } :
      { status: "skipped", attemptId: "fixture", origin: "turn_complete", durationMs: 1,
        ...(subtype === "memory_generation" ? { writtenFiles: [], failedFiles: [] } : { suggestions: [] }) };
    const callback = async value => {
      assert.deepEqual(value, result);
      entered.resolve();
      try { await release.promise; } finally { finalized = true; }
    };
    const extra = subtype === "skill_evolution" ?
      { evolution: { skill: { mode: "native", generation: { onResult: callback, turnComplete: { reviewThreshold: 7 } } } } } :
      { memory: { mode: "native", projectScope: false, userScope: true,
        [subtype === "memory_generation" ? "generation" : "consumption"]: { onResult: callback } } };
    const f = await fixture(t, undefined, extra);
    if (subtype === "skill_evolution") {
      assert.equal(f.transport.initialization.evolution.skill.mode, "native");
      assert.equal(f.transport.initialization.evolution.skill.generation.turnComplete.reviewThreshold, 7);
    } else {
      assert.deepEqual(f.transport.initialization.memory,
        { requester: "sdk", mode: "native", projectScope: false, userScope: true });
      assert.equal(extra.memory[subtype === "memory_generation" ? "generation" : "consumption"].onResult, callback);
    }
    f.transport.emit({ type: "system", subtype, session_id: "session", uuid: subtype, result });
    await entered.promise;
    let closed = false;
    const closing = f.query.close().then(() => { closed = true; });
    try {
      await pause();
      assert.equal(closed, false);
      assert.equal(f.scope.closed, false);
    } finally {
      release.resolve();
      await closing;
    }
    assert.equal(finalized, true);
    assert.equal(f.scope.closed, true);
  });
}

test("native sendHostAction fallback finalizer drains before scope release", { timeout: 5000 }, async t => {
  const f = await fixture(t, undefined, { hostActions: {} });
  const entered = deferred(), release = deferred();
  let finalized = false, calls = 0;
  const action = { uuid: "00000000-0000-4000-8000-000000000001", action: "helpful",
    sessionId: "session", parentResultUuid: "00000000-0000-4000-8000-000000000002" };
  // The SDK creates this CLI helper internally; exercise its real public dispatch with a held helper.
  f.resident.query.hostActionFallback = async event => {
    calls++;
    assert.deepEqual(event, { type: "feedback", subtype: "action", uuid: action.uuid,
      action: "helpful", session_id: "session", parent_result_uuid: action.parentResultUuid });
    entered.resolve();
    try { await release.promise; } finally { finalized = true; }
  };
  f.transport.isReady = () => false;
  const sending = f.query.sendHostAction(action);
  await Promise.race([entered.promise, sending.then(() => assert.fail("native fallback was skipped"))]);
  let closed = false;
  const closing = f.query.close().then(() => { closed = true; });
  try {
    await pause();
    assert.equal(closed, false);
    assert.equal(f.scope.closed, false);
  } finally {
    release.resolve();
    await Promise.all([sending, closing]);
  }
  assert.equal(finalized, true);
  assert.equal(f.scope.closed, true);
  await assert.rejects(async () => f.query.sendHostAction(action), /closed|unavailable/);
  assert.equal(calls, 1);
});

test("callback decoration preserves native invalid-option rejection and untouched containers", () => {
  const resident = new Resident({ hooks: [] });
  const untouched = { memory: { mode: "custom", generation: { enabled: false } },
    evolution: { skill: { mode: "native", generation: { enabled: false } } } };
  const snapshot = structuredClone(untouched);
  const decorated = resident.decorate(untouched);
  assert.equal(decorated.memory, untouched.memory);
  assert.equal(decorated.evolution, untouched.evolution);
  assert.deepEqual(untouched, snapshot);
  let creates = 0;
  const transport = { create: () => { creates++; throw new Error("invalid options reached transport"); } };
  const callback = async () => {};
  const invalid = [{ memory: [] },
    { memory: Object.assign([], { mode: "custom", generation: { onResult: callback } }) },
    { memory: { mode: "custom", generation: "bad" } },
    { memory: { mode: "custom", consumption: "bad" } },
    { memory: { mode: "custom", generation: { turnComplete: "bad" } } },
    { memory: { mode: "custom", generation: "bad", consumption: { onResult: callback } } },
    { evolution: { skill: "bad" } },
    { evolution: { skill: { mode: "custom", generation: "bad" } } },
    { evolution: { skill: { mode: "custom", generation: { turnComplete: "bad" } } } }];
  for (const extra of invalid) {
    const options = { transport, auth: { type: "accessToken", accessToken: "fixture-local-only" }, ...extra };
    let message;
    assert.throws(() => sdk.query({ prompt: "fixture", options }), error => {
      assert(error instanceof TypeError);
      message = error.message;
      return true;
    });
    assert.throws(() => sdk.query({ prompt: "fixture", options: resident.decorate(options) }),
      { name: "TypeError", message });
  }
  assert.equal(creates, 0);
});

test("managed MCP rejects required model parent before native server creation", () => {
  let creations = 0;
  const native = { createSdkMcpServer(options) { creations++; return sdk.createSdkMcpServer(options); } };
  assert.throws(() => managedMcpServer(native, { name: "fixture", tools: [],
    parentIdentity: "required" }), /lacks a trusted model parent/);
  assert.equal(creations, 0);
});

test("managed MCP snapshots handler and exposes only proven query identity", { timeout: 5000 }, async t => {
  const policies = [];
  let original = 0, replaced = 0;
  const definition = { name: "read", description: "Read fixture", inputSchema: {},
    handler: async () => { original++; return { content: [], structuredContent: { original: true } }; } };
  const server = managedMcpServer(sdk, { name: "fixture", tools: [definition] });
  definition.handler = async () => { replaced++; return { content: [] }; };
  const f = await fixture(t, undefined, { mcpServers: { fixture: server } });
  f.resident.config.hooks = policyHooks;
  f.scope.invoke = async (hook, payload) => {
    policies.push(structuredClone(payload));
    if (hook.event === "tool.after") payload.tool_response.result.structuredContent.original = false;
    return { code: 0, stdout: Buffer.from("{}") };
  };
  f.transport.emit({ type: "control_request", request_id: "snapshot", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1,
      method: "tools/call", params: { name: "read", arguments: {} } } } });
  assert.deepEqual((await f.response("snapshot")).response.mcp_response.result.structuredContent, { original: true });
  assert.equal(original, 1);
  assert.equal(replaced, 0);
  assert.equal(policies.length, 2);
  assert.deepEqual(policies[0].aw_mcp_context, { server: "fixture", tool: "read",
    subject: "mcp__fixture__read", parent_tool_call_id: null, parent_query_id: f.resident.id,
    generation: f.scope.execution, identity_scope: "query" });
  assert.deepEqual(policies[1].aw_mcp_context, policies[0].aw_mcp_context);
});

test("managed MCP cancellation during Pre prevents actual handler entry", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  let calls = 0;
  const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read",
    description: "Read fixture", inputSchema: {}, handler: async () => { calls++; return { content: [] }; } }] });
  const f = await fixture(t, undefined, { mcpServers: { fixture: server } });
  f.resident.config.hooks = [policyHooks[0]];
  f.scope.invoke = async () => {
    entered.resolve();
    await release.promise;
    return { code: 0, stdout: Buffer.from("{}") };
  };
  f.transport.emit({ type: "control_request", request_id: "cancel-pre", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1,
      method: "tools/call", params: { name: "read", arguments: {} } } } });
  await entered.promise;
  f.transport.emit({ type: "control_cancel_request", request_id: "cancel-pre" });
  await pause();
  release.resolve();
  await f.resident.close();
  assert.equal(f.transport.responses.has("cancel-pre"), false);
  assert.equal(calls, 0);
  assert.equal(f.scope.invocations.size, 0);
});

test("managed MCP generation changes during Pre prevent native execution", { timeout: 5000 }, async t => {
  let calls = 0;
  const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read",
    description: "Read fixture", inputSchema: {}, handler: async () => { calls++; return { content: [] }; } }] });
  const f = await fixture(t, undefined, { mcpServers: { fixture: server } });
  f.resident.config.hooks = [policyHooks[0]];
  f.scope.invoke = async () => {
    f.scope.execution++;
    return { code: 0, stdout: Buffer.from("{}") };
  };
  f.transport.emit({ type: "control_request", request_id: "stale", request: {
    subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1,
      method: "tools/call", params: { name: "read", arguments: {} } } } });
  assert.equal((await f.response("stale")).subtype, "error");
  assert.equal(calls, 0);
  assert.equal(f.scope.invocations.size, 0);
});
