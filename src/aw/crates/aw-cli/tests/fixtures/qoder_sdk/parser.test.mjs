/** Exercise the pinned SDK's real control parser without credentials or a model connection. */
import test from "node:test";
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { Resident } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";
import { managedMcpServer, admitMcp } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";
import { invoke } from "../../../../aw-adapter/native/qoder-sdk/callback.mjs";
const sdk = await import(pathToFileURL(process.argv[2]).href);
const { z } = createRequire(pathToFileURL(process.argv[2]))("zod");
const pause = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

class Transport {
  messages = [];
  responses = new Map();
  changed = deferred();
  closed = false;
  hooks = [];
  async initialize() {}
  isReady() { return !this.closed; }
  emit(message) { this.messages.push(message); this.changed.resolve(); }
  write(line) {
    const message = JSON.parse(line);
    if (message.type === "control_request") {
      if (message.request.type === "initialize") {
        this.initialization = message.request;
        this.hooks = message.request.hooks;
      }
      if (message.request.type === "get_context_usage" && this.holdBarrier) {
        this.barrier = message;
        return;
      }
      this.emit({ type: "control_response", response: { subtype: "success",
        request_id: message.request_id, response: { capabilities: ["msg_lifecycle_v1", "host_action"] } } });
    } else if (message.type === "control_response") {
      this.responses.set(message.response.request_id, message.response);
    }
  }
  async *readMessages() {
    while (!this.closed) {
      if (!this.messages.length) { await this.changed.promise; this.changed = deferred(); }
      if (this.failure) throw this.failure;
      while (this.messages.length) yield this.messages.shift();
    }
  }
  endInput() {}
  close() { this.closed = true; this.changed.resolve(); }
}

async function fixture(t, callback = async () => ({}), extra = {}, policyHooks) {
  const transport = new Transport();
  const resident = new Resident({ hooks: policyHooks ?? [], sdk: process.argv[2] });
  const scope = resident.scope;
  scope.open = async () => { scope.handle = {}; };
  scope.invocationLimit = 4;
  scope.register = async (id, name, input) => { scope.invocations.set(id, { name, input }); };
  scope.finish = async id => { assert(scope.invocations.delete(id)); };
  scope.close = async () => { scope.closed = true; };
  const input = deferred();
  async function* prompt() { await input.promise; }
  const options = { transport: { create: () => transport },
    auth: { type: "accessToken", accessToken: "fixture-local-only" }, ...extra,
    hooks: { PreToolUse: [{ hooks: [callback] }], PostToolUse: [{ hooks: [callback] }] } };
  if (policyHooks) {
    await resident.create(prompt(), options, (config, native) => {
      const hooks = Object.fromEntries(Object.entries(native.hooks).map(([event, groups]) => [event, [...groups]]));
      for (const hook of config.hooks) {
        const event = hook.event === "tool.before" ? "PreToolUse" : "PostToolUse";
        hooks[event].push({ hooks: [async (input, id, { signal }) => {
          assert.equal(id, input.tool_use_id);
          return await invoke(config, hook, input, signal);
        }] });
      }
      return { ...native, hooks };
    }, () => {});
  } else {
    resident.mcp = admitMcp(extra);
    resident.cwd = process.cwd();
    await scope.open();
    scope.begin();
    const query = sdk.query({ prompt: prompt(), options: resident.decorate(options) });
    resident.install(query);
    await query.initializationResult();
  }
  t.after(async () => { input.resolve(); await resident.close(); });
  const model = (id, input = { command: "fixture" }, name = "Bash") => transport.emit({ type: "assistant",
    session_id: "session", uuid: `message-${id}`, parent_tool_use_id: null,
    message: { role: "assistant", content: [{ type: "tool_use", id, name, input }] } });
  const hook = (id, request, event = "PreToolUse", input = { command: "fixture" },
    { name = "Bash", mcpContext, group = 0 } = {}) => {
    const callbackId = transport.hooks[event][group].hookCallbackIds[0];
    transport.emit({ type: "control_request", request_id: request,
      request: { subtype: "hook_callback", callback_id: callbackId, tool_use_id: id,
        input: { hook_event_name: event, tool_use_id: id, tool_name: name, tool_input: input,
          ...(mcpContext ? { mcp_context: mcpContext } : {}),
          cwd: process.cwd(), session_id: "session", tool_response: "result" } } });
  };
  const response = async request => {
    for (let attempt = 0; attempt < 1000; attempt++) {
      if (transport.responses.has(request)) return transport.responses.get(request);
      await pause();
    }
    throw new Error(`Native response timeout: ${request}`);
  };
  return { resident, scope, query: resident.facade(), transport, model, hook, response };
}

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

test("managed MCP deny prevents handlers and injected strict Post defends the bridge", { timeout: 5000 }, async t => {
  for (const event of ["tool.before", "tool.after"]) {
    let calls = 0;
    const server = managedMcpServer(sdk, { name: "fixture", tools: [{ name: "read", description: "Read fixture", inputSchema: {},
      handler: async () => { calls++; return { content: [{ type: "text", text: "private result" }] }; } }] });
    const f = await fixture(t, undefined, { mcpServers: { fixture: server } });
    // Strict Post is injected below admission: qoder-sdk admits only observe/report
    // after a tool. Strict Post policy admission remains unsupported.
    f.resident.config.hooks = [{ event, step: "required", on_error: "block", budget_ms: 100 }];
    f.scope.invoke = async () => event === "tool.before" ?
      { code: 2, stdout: Buffer.from('"policy denied"') } : { code: 1, stdout: Buffer.from("") };
    f.transport.emit({ type: "control_request", request_id: event, request: {
      subtype: "mcp_message", server_name: "fixture", message: { jsonrpc: "2.0", id: 1,
        method: "tools/call", params: { name: "read", arguments: {} } } } });
    const response = await f.response(event);
    assert.equal(response.subtype, "error");
    assert(!JSON.stringify(response).includes("private result"));
    assert.equal(calls, event === "tool.before" ? 0 : 1);
    assert.equal(f.scope.invocations.size, 0);
    if (event === "tool.after") assert(f.resident.failure);
  }
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
  ({ event, step: "required", on_error: "block", budget_ms: 100 }));

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
