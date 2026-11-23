/** Native Hooks retain rewrites; AW records the input observed at its public callback. */
import test from "node:test";
import assert from "node:assert/strict";
import { fixture, pause, sdk, z } from "./parser-support.mjs";
import { policyCallback } from "../../../../aw-adapter/native/qoder-sdk/native-configuration.mjs";

const profile = { resident: true };
const policyHooks = callback => ({ PreToolUse: [{ matcher: "*", hooks: [policyCallback(callback)] }] });

test("public Hook registration binds observed input without a patched Worker marker", async t => {
  let checked;
  const f = await fixture(t, undefined, { hooks: policyHooks(async input => { checked = input.tool_input; return {}; }) }, undefined, profile);
  f.model("call", { command: "model" });
  await pause();
  assert.equal(f.scope.invocations.size, 0);
  f.hook("call", "pre", "PreToolUse", { command: "observed" });
  assert.deepEqual((await f.response("pre")).response, {});
  assert.deepEqual(checked, { command: "observed" });
  assert.deepEqual([...f.scope.invocations.values()][0].input, checked);
  await f.resident.identities.retire("call");
  assert.equal(f.scope.invocations.size, 0);
});

test("native denial can retire an invocation without an AW callback", async t => {
  const f = await fixture(t, undefined, { hooks: policyHooks(async () => ({})) }, undefined, profile);
  f.model("denied");
  await pause();
  await f.resident.identities.retire("denied");
  assert.equal(f.scope.invocations.size, 0);
});

for (const mode of ["invalid-input", "repeat", "policy-deny", "mapping-failure"]) {
  test(`public Hook fails closed for ${mode}`, async t => {
    const f = await fixture(t, undefined, { hooks: policyHooks(async () => mode === "policy-deny" ?
      { hookSpecificOutput: { permissionDecision: "deny" } } : {}) }, undefined, profile);
    f.model("call");
    if (mode === "mapping-failure") f.scope.register = async () => { throw new Error("mapping failed"); };
    f.hook("call", "first", "PreToolUse", mode === "invalid-input" ? null : { command: "observed" });
    const first = await f.response("first");
    if (mode === "repeat") {
      assert.deepEqual(first.response, {});
      f.hook("call", "second", "PreToolUse", { command: "different" });
      assert.equal((await f.response("second")).response.hookSpecificOutput.permissionDecision, "deny");
    } else assert.equal(first.response.hookSpecificOutput.permissionDecision, "deny");
  });
}

test("native rewrites remain visible to application Post while policy retains its observed snapshot", async t => {
  let post;
  const f = await fixture(t, undefined, { hooks: {
    PreToolUse: [
      { matcher: "Bash", hooks: [async () => ({ hookSpecificOutput: {
        hookEventName: "PreToolUse", updatedInput: { command: "rewritten" },
      } })] }, ...policyHooks(async () => ({})).PreToolUse,
    ],
    PostToolUse: [{ hooks: [async input => { post = input.tool_input; return {}; }] }],
  } }, undefined, profile);
  f.model("call", { command: "original" });
  f.hook("call", "native", "PreToolUse", { command: "original" });
  assert.deepEqual((await f.response("native")).response.hookSpecificOutput.updatedInput, { command: "rewritten" });
  f.hook("call", "policy", "PreToolUse", { command: "original" }, { group: 1 });
  assert.deepEqual((await f.response("policy")).response, {});
  f.hook("call", "post", "PostToolUse", { command: "rewritten" });
  assert.deepEqual((await f.response("post")).response, {});
  assert.deepEqual(post, { command: "rewritten" });
  assert.deepEqual([...f.scope.invocations.values()][0].input, { command: "original" });
});

for (const blocked of [false, true]) {
  test(`native SDK MCP checks parsed input and preserves transforms, blocked=${blocked}`, async t => {
    let called = 0;
    const server = sdk.createSdkMcpServer({ name: "native", tools: [{ name: "run", description: "run",
      inputSchema: { command: z.string().transform(() => "transformed") },
      handler: async input => { called++; assert.equal(input.command, "transformed"); return { content: [] }; } }] });
    const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
    f.resident.config.hooks = [{ event: "tool.before", step: "check", on_error: "block" }];
    f.scope.invoke = async (_hook, payload) => {
      assert.deepEqual(payload.tool_input, { command: "transformed" });
      return { code: blocked ? 2 : 0, stdout: Buffer.from(blocked ? '"denied"' : '{}') };
    };
    f.transport.emit({ type: "control_request", request_id: "native-call", request: {
      subtype: "mcp_message", server_name: "native", message: { jsonrpc: "2.0", id: 1,
        method: "tools/call", params: { name: "run", arguments: { command: "original" } } } } });
    const response = await f.response("native-call");
    assert.equal(called, blocked ? 0 : 1);
    assert.equal(response.subtype, blocked ? "error" : "success");
    assert.equal(f.scope.invocations.size, 0);
  });
}

const nativeCall = (f, id, name = "run", args = {}) => f.transport.emit({ type: "control_request", request_id: id,
  request: { subtype: "mcp_message", server_name: "native", message: {
    jsonrpc: "2.0", id, method: "tools/call", params: { name, arguments: args } } } });
const rawServer = handler => sdk.createSdkMcpServer({ name: "native", tools: [
  { name: "run", description: "run", inputSchema: {}, handler },
] });

test("native MCP errors pass through strict Post before response delivery", async t => {
  const server = rawServer(async () => { throw new Error("private-output"); });
  const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
  f.resident.config.hooks = [{ event: "tool.after", step: "post", on_error: "withhold_result" }];
  let posts = 0;
  f.scope.invoke = async (_hook, payload) => {
    posts++;
    assert.equal(payload.tool_response.result.isError, true);
    return { code: 1, stdout: Buffer.alloc(0) };
  };
  nativeCall(f, "error");
  for (let i = 0; i < 1000 && !f.transport.closed; i++) await pause();
  assert.equal(posts, 1);
  assert.equal(f.transport.closed, true);
  assert.equal(f.transport.responses.has("error"), false);
});

test("native MCP cancellation retains scope until the handler finalizer settles", async t => {
  let enter, finish;
  const entered = new Promise(resolve => { enter = resolve; });
  const done = new Promise(resolve => { finish = resolve; });
  const server = rawServer(async () => { enter(); await done; return { content: [] }; });
  const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
  nativeCall(f, "cancel");
  await entered;
  f.transport.emit({ type: "control_cancel_request", request_id: "cancel" });
  let closed = false;
  const closing = f.resident.close().then(() => { closed = true; });
  try {
    await pause();
    assert.equal(closed, false);
    assert.equal(f.scope.invocations.size, 1);
  } finally { finish(); await closing; }
  assert.equal(f.scope.invocations.size, 0);
});

test("native MCP reuse and handler updates retain per-Query admission", async t => {
  let calls = 0;
  const server = rawServer(async () => { calls++; return { content: [] }; });
  for (const id of ["first", "second"]) {
    const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
    if (id === "second") server.instance._registeredTools.run.update({ callback: async () => {
      calls += 10; return { content: [] };
    } });
    nativeCall(f, id);
    const response = await f.response(id);
    assert.equal(response.subtype, "success", JSON.stringify(response));
    await f.resident.close();
    assert.equal(f.scope.invocations.size, 0);
  }
  assert.equal(calls, 11);
});


for (const blocked of [false, true]) {
  test(`native MCP tools without input schemas retain request context, blocked=${blocked}`, async t => {
    let calls = 0;
    const server = sdk.createSdkMcpServer({ name: "native", tools: [] });
    server.instance.registerTool("run", { description: "no arguments" }, async (...args) => {
      calls++;
      assert.equal(args.length, 1);
      assert.equal(typeof args[0].sendNotification, "function");
      assert(args[0].signal instanceof AbortSignal);
      return { content: [{ type: "text", text: "done" }] };
    });
    const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
    f.resident.config.hooks = [
      { event: "tool.before", step: "check", on_error: "block" },
      { event: "tool.after", step: "post", on_error: "withhold_result" },
    ];
    const events = [];
    f.scope.invoke = async (hook, payload) => {
      events.push(hook.event);
      assert.deepEqual(payload.tool_input, {});
      if (hook.event === "tool.after")
        assert.deepEqual(payload.tool_response.result.content, [{ type: "text", text: "done" }]);
      return { code: blocked ? 2 : 0, stdout: Buffer.from(blocked ? '"denied"' : '{}') };
    };
    nativeCall(f, "no-input", "run", { ignoredByNativeHandler: true });
    const response = await f.response("no-input");
    assert.equal(response.subtype, blocked ? "error" : "success", JSON.stringify(response));
    assert.equal(calls, blocked ? 0 : 1);
    assert.deepEqual(events, blocked ? ["tool.before"] : ["tool.before", "tool.after"]);
    assert.equal(f.scope.invocations.size, 0);
  });
}

test("native MCP registration renames retain handler identity and policy selection", async t => {
  let calls = 0, blocked = false;
  const server = sdk.createSdkMcpServer({ name: "native", tools: [] });
  const registration = server.instance.registerTool("run", { inputSchema: {} }, async () => {
    calls++; return { content: [] };
  });
  const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
  f.resident.config.hooks = [
    { event: "tool.before", step: "check", on_error: "block" },
    { event: "tool.after", step: "post", on_error: "withhold_result" },
  ];
  const checks = [], posts = [];
  f.scope.invoke = async (hook, payload) => {
    (hook.event === "tool.before" ? checks : posts).push(payload.aw_mcp_context.tool);
    assert.equal(payload.tool_name, `mcp__native__${payload.aw_mcp_context.tool}`);
    return { code: blocked ? 2 : 0, stdout: Buffer.from(blocked ? '"denied"' : '{}') };
  };
  nativeCall(f, "original");
  assert.equal((await f.response("original")).subtype, "success");
  assert.equal(calls, 1);
  registration.update({ name: "renamed" });
  blocked = true;
  nativeCall(f, "blocked", "renamed");
  assert.equal((await f.response("blocked")).subtype, "error");
  assert.equal(calls, 1);
  blocked = false;
  nativeCall(f, "allowed", "renamed");
  const response = await f.response("allowed");
  assert.equal(response.subtype, "success", JSON.stringify(response));
  assert.equal(response.response.mcp_response.result.isError, undefined);
  assert.equal(calls, 2);
  assert.deepEqual(checks, ["run", "renamed", "renamed"]);
  assert.deepEqual(posts, ["run", "renamed"]);
  assert.equal(f.scope.invocations.size, 0);
});

for (const withSchema of [false, true]) {
  test(`native MCP registration replacement cannot precede policy denial, schema=${withSchema}`, async t => {
    for (let turns = 0; turns <= 20; turns++) {
      const events = [];
      const server = sdk.createSdkMcpServer({ name: "native", tools: [] });
      const config = withSchema ? { inputSchema: {} } : {};
      const handler = async () => { events.push("handler"); return { content: [] }; };
      const registration = server.instance.registerTool("run", config, handler);
      const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
      f.resident.config.hooks = [{ event: "tool.before", step: "check", on_error: "block" }];
      f.scope.invoke = async () => {
        events.push("denied");
        return { code: 2, stdout: Buffer.from('"denied"') };
      };
      nativeCall(f, `replace-${turns}`);
      for (let tick = 0; tick < turns; tick++) await Promise.resolve();
      registration.remove();
      server.instance.registerTool("run", config, handler);
      events.push("replaced");
      const response = await f.response(`replace-${turns}`);
      assert.equal(response.subtype, "error", JSON.stringify(response));
      assert(!events.includes("handler"), `turns=${turns}: ${events.join(" -> ")}`);
      assert.equal(f.scope.invocations.size, 0);
      await f.resident.close();
    }
  });
}

test("native MCP fresh registration between calls retains policy and native execution", async t => {
  const events = [];
  const server = sdk.createSdkMcpServer({ name: "native", tools: [] });
  const registration = server.instance.registerTool("run", { inputSchema: {} }, async () => {
    events.push("original"); return { content: [] };
  });
  const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
  f.resident.config.hooks = [{ event: "tool.before", step: "check", on_error: "block" }];
  f.scope.invoke = async () => { events.push("checked"); return { code: 0, stdout: Buffer.from('{}') }; };
  nativeCall(f, "before-refresh");
  assert.equal((await f.response("before-refresh")).subtype, "success");
  registration.remove();
  server.instance.registerTool("run", { inputSchema: {} }, async () => {
    events.push("replacement"); return { content: [] };
  });
  nativeCall(f, "after-refresh");
  assert.equal((await f.response("after-refresh")).subtype, "success");
  assert.deepEqual(events, ["checked", "original", "checked", "replacement"]);
  assert.equal(f.scope.invocations.size, 0);
});

test("native MCP input rejection preserves its error without entering a handler", async t => {
  let calls = 0;
  const server = sdk.createSdkMcpServer({ name: "native", tools: [{ name: "run", description: "run",
    inputSchema: { command: z.string() }, handler: async () => { calls++; return { content: [] }; } }] });
  const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
  f.resident.config.hooks = [
    { event: "tool.before", step: "check", on_error: "block" },
    { event: "tool.after", step: "post", on_error: "withhold_result" },
  ];
  const events = [];
  f.scope.invoke = async (hook, payload) => {
    events.push(hook.event);
    if (hook.event === "tool.after") assert.equal(payload.tool_response.result.isError, true);
    return { code: 0, stdout: Buffer.from('{}') };
  };
  nativeCall(f, "invalid", "run", { command: 12 });
  const response = await f.response("invalid");
  assert.equal(response.subtype, "success", JSON.stringify(response));
  assert.equal(response.response.mcp_response.result.isError, true);
  assert.equal(calls, 0);
  assert.deepEqual(events, ["tool.before", "tool.after"]);
  assert.equal(f.scope.invocations.size, 0);
});

test("native MCP callback updates during Pre apply only to the next handler admission", async t => {
  const events = [];
  const server = sdk.createSdkMcpServer({ name: "native", tools: [] });
  const registration = server.instance.registerTool("run", { inputSchema: {} }, async () => {
    events.push("original"); return { content: [] };
  });
  const f = await fixture(t, undefined, { mcpServers: { native: server } }, undefined, profile);
  f.resident.config.hooks = [{ event: "tool.before", step: "check", on_error: "block" }];
  f.scope.invoke = async () => {
    events.push("checked");
    registration.update({ callback: async () => { events.push("updated"); return { content: [] }; } });
    await pause();
    return { code: 0, stdout: Buffer.from('{}') };
  };
  for (const id of ["first", "second"]) {
    nativeCall(f, id);
    assert.equal((await f.response(id)).subtype, "success");
  }
  assert.deepEqual(events, ["checked", "original", "checked", "updated"]);
  assert.equal(f.scope.invocations.size, 0);
});
