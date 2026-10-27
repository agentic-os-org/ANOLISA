/** Task policy handoff must precede native routes and outlive original cleanup. */
import test from "node:test";
import assert from "node:assert/strict";
import { fixture, deferred } from "./resident-support.mjs";
import { Scope } from "../../../../aw-adapter/native/shared/scope.mjs";
import { managedMcpServer, dispatchMcp } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";

const route = (step, event = "tool.before", on_error = "block") =>
  ({ event, step, on_error, budget_ms: 500, replace_result: false });
const policy = (hooks, revision = "a") => ({ hooks, events: {}, policy_revision: revision.repeat(64) });

function reload(f, snapshot) {
  f.config.resident_policy_reload = true;
  const calls = [];
  f.scope.reserve = async () => { calls.push("reserve"); f.scope.reservation = {}; };
  f.scope.open = async (_cwd, _env, options) => {
    assert.deepEqual(options, { deferAcknowledgement: true });
    calls.push("open");
    f.scope.handle = {};
  };
  f.scope.policy = async () => { calls.push("policy"); return snapshot; };
  f.scope.acknowledge = async () => { calls.push("acknowledge"); };
  f.scope.release = async () => {
    assert.equal(f.scope.closed, true);
    calls.push("release");
    f.scope.reservation = null;
  };
  return calls;
}

test("each new Query adopts added and removed Hooks before native initialization", { timeout: 5000 }, async t => {
  for (const [hooks, strict] of [
    [[route("scan"), route("strict", "tool.after", "withhold_result")], true],
    [[], false],
    [[route("observe", "tool.after", "report")], false],
  ]) {
    const f = fixture(t);
    f.config.hooks = [route("obsolete", "tool.after", "withhold_result")];
    const snapshot = policy(hooks);
    const calls = reload(f, snapshot);
    let selected;
    const pending = f.create({}, (config, options) => {
      calls.push("compose");
      selected = config;
      assert.equal(f.resident.strictPost, strict);
      return { ...options, hooks: {} };
    });
    await f.state.imported.promise;
    assert.deepEqual(calls, ["reserve", "open", "policy", "compose", "acknowledge"]);
    assert.deepEqual(selected.hooks, hooks);
    assert.equal(f.state.created, 0);
    assert.equal(selected.policy_revision, "a".repeat(64));
    assert(Object.isFrozen(selected.hooks));
    assert.deepEqual(f.config.hooks, [route("obsolete", "tool.after", "withhold_result")]);
    const selectedCount = hooks.length;
    snapshot.hooks.push(route("injected-after-handoff"));
    assert.equal(selected.hooks.length, selectedCount, "handoff owns an immutable copy");
    f.state.importGate.resolve();
    assert((await pending).value);
    assert.equal(f.state.readyConfig, f.config, "startup receipt retains launch configuration");
    assert.deepEqual(f.state.readyConfig.hooks, [route("obsolete", "tool.after", "withhold_result")]);
    f.state.closeGate.resolve();
    await f.resident.close();
    assert.equal(calls.at(-1), "release");
  }
});

test("malformed task snapshots fail before composition or acknowledgement", { timeout: 5000 }, async t => {
  for (const snapshot of [null, {}, policy(null), policy([null]), policy([route("", "tool.before")]),
    policy([{ ...route("scan"), event: "unknown" }]),
    policy([{ ...route("scan"), on_error: "ignore" }]),
    policy([{ ...route("scan"), budget_ms: 0 }]),
    policy([{ ...route("scan"), budget_ms: 1.5 }]),
    policy([{ ...route("scan"), replace_result: true }]),
    { ...policy([]), policy_revision: "bad" }]) {
    const f = fixture(t);
    const original = f.resident.config;
    const calls = reload(f, snapshot);
    const result = await f.create({}, () => { calls.push("compose"); return {}; });
    assert(result.error);
    assert.equal(f.resident.config, original);
    assert.equal(f.state.created, 0);
    assert(!calls.includes("compose"));
    assert(!calls.includes("acknowledge"));
    assert.equal(calls.at(-1), "release");
  }
});

test("task snapshots cannot replace launch identity or executable", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const snapshot = { ...policy([]), executable: "injected", binding: "injected", resident: false };
  reload(f, snapshot);
  f.state.importGate.resolve();
  assert((await f.create()).value);
  assert.equal(f.resident.config.executable, f.config.executable);
  assert.equal(f.resident.config.binding, f.config.binding);
  assert.equal(f.resident.config.resident, f.config.resident);
  f.state.closeGate.resolve();
  await f.resident.close();
});

test("native shutdown and original callback finalizers keep the root claim busy", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const calls = reload(f, policy([]));
  f.state.importGate.resolve();
  assert((await f.create()).value);
  const callback = deferred();
  f.resident.track(() => callback.promise);
  const closing = f.resident.close();
  await f.state.closeEntered.promise;
  assert(!calls.includes("release"));
  f.state.closeGate.resolve();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.state.scopeCloses, 0);
  assert(!calls.includes("release"));
  callback.resolve();
  await closing;
  assert.equal(calls.at(-1), "release");
});

test("unconfirmed native cleanup retains the root claim after failure", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const calls = reload(f, policy([]));
  delete f.state.query.close;
  f.state.importGate.resolve();
  assert((await f.create()).error);
  assert(!calls.includes("release"));
  assert(f.scope.reservation);
  assert.equal(f.scope.closed, false);
});

test("route composition failure releases admitted state without native dispatch", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const calls = reload(f, policy([]));
  const result = await f.create({}, () => { throw new Error("route preparation rejected"); });
  assert.match(result.error.message, /route preparation/);
  assert.equal(f.state.created, 0);
  assert(!calls.includes("acknowledge"));
  assert.equal(calls.at(-1), "release");
});

test("managed MCP uses the task snapshot instead of launch routes", { timeout: 5000 }, async t => {
  const f = fixture(t);
  f.config.hooks = [route("obsolete")];
  reload(f, policy([route("new-before"), route("new-after", "tool.after", "report")], "b"));
  let handler, executions = 0;
  const server = managedMcpServer({ createSdkMcpServer: options => {
    handler = options.tools[0].handler;
    return { type: "sdk" };
  } }, { tools: [{ name: "read", handler: async () => {
    executions++;
    return { content: [{ type: "text", text: "structured receipt" }], isError: false };
  } }] });
  const invoked = [];
  f.scope.register = async id => { f.scope.invocations.set(id, 1); };
  f.scope.finish = async id => { f.scope.invocations.delete(id); };
  f.scope.invoke = async hook => { invoked.push(hook.step); return { code: 0, stdout: "{}" }; };
  f.state.importGate.resolve();
  assert((await f.create({ mcpServers: { files: server } })).value);
  const frame = { request_id: "request", request: { subtype: "mcp_message", server_name: "files",
    message: { method: "tools/call", params: { name: "read", arguments: { path: "fixture" } } } } };
  const ticket = { kind: "mcp_message", id: "request", request: structuredClone(frame.request) };
  const result = await f.resident.identities.dispatch.run(ticket, () => dispatchMcp(f.resident,
    frame, new AbortController().signal, async () => ({ mcp_response: await handler({ path: "fixture" }) })));
  assert.deepEqual(invoked, ["new-before", "new-after"]);
  assert.equal(executions, 1);
  assert.equal(result.mcp_response.content[0].text, "structured receipt");
  assert.equal(f.resident.config.policy_revision, "b".repeat(64));
  f.state.closeGate.resolve();
  await f.resident.close();
});

test("shared Scope preserves explicit reservation until confirmed child cleanup", async () => {
  const scope = new Scope({});
  const calls = [];
  scope.command = async action => {
    calls.push(action);
    return { code: 0, stdout: JSON.stringify(action === "enable" ? { invocations_per_binding: 4 } : {}) };
  };
  await scope.reserve();
  await scope.open(process.cwd(), {}, { deferAcknowledgement: true });
  assert.deepEqual(calls, ["reserve", "open"]);
  await assert.rejects(scope.release(), /cleanup is not confirmed/);
  await scope.policy();
  await scope.acknowledge();
  await scope.close();
  await scope.release();
  assert.deepEqual(calls, ["reserve", "open", "policy", "acknowledge", "enable", "close", "release"]);
  assert.equal(scope.reservation, null);
});
