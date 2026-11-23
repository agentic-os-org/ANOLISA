/** Replay Worker child producer ordering through the pinned SDK control parser. */
import test from "node:test";
import assert from "node:assert/strict";
import { fixture, pause, deferred } from "./parser-support.mjs";

const input = { command: "fixture" };
const parentInput = { description: "fixture", prompt: "fixture", subagent_type: "general-purpose" };
const until = async (predicate, reason) => {
  for (let attempt = 0; attempt < 1000; attempt++) {
    if (predicate()) return;
    await pause();
  }
  assert.fail(reason);
};
const start = (f, id = "child", { parent = "parent", session = "session", name = "Bash" } = {}) =>
  f.transport.emit({ type: "stream_event", session_id: session, parent_tool_use_id: parent,
    event: { type: "content_block_start", index: 0,
      content_block: { type: "tool_use", id, name, input: {} } } });
const parent = async (f, id = "parent", name = "Agent") => {
  f.model(id, parentInput, name);
  f.hook(id, `${id}-pre`, "PreToolUse", parentInput, { name });
  await f.response(`${id}-pre`);
};
const waiting = f => until(() => f.resident.identities.active.get("child")?.callbacks.size === 1,
  "child Hook must reach its producer wait");
const denied = response => assert.equal(response.response.hookSpecificOutput.permissionDecision, "deny");
const retired = async (f, id) => {
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: id }] } });
  await until(() => !f.resident.identities.active.has(id), "native result must retire its identity");
};
const heldResult = async (f, id) => {
  f.transport.holdBarrier = true;
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: id }] } });
  await until(() => f.transport.barrier, "result must wait for the native terminal queue barrier");
  const entry = f.resident.identities.active.get(id);
  assert.equal(entry.terminal, true);
  assert(f.scope.invocations.has(entry.id), "terminal identity must remain registered before the barrier");
};
const releaseResult = f => {
  f.transport.holdBarrier = false;
  f.transport.write(JSON.stringify(f.transport.barrier));
};
const drained = async f => {
  await f.query.close();
  assert.equal(f.scope.invocations.size, 0);
  assert.equal(f.resident.identities.active.size, 0);
  assert.equal(f.resident.identities.pending.size, 0);
  assert.equal(f.resident.callbacks.size, 0);
  assert.equal(f.resident.work.size, 0);
};

test("child Pre awaits the complete assistant while the real SDK keeps reading", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (payload, id) => { calls.push([payload.hook_event_name, id]); return {}; });
  await parent(f);
  start(f);
  f.transport.emit({ type: "stream_event", session_id: "session", parent_tool_use_id: "parent",
    event: { type: "content_block_delta", index: 0, delta: { type: "input_json_delta", partial_json: "untrusted" } } });
  f.hook("child", "child-pre");
  await waiting(f);
  assert.equal(f.transport.responses.has("child-pre"), false);
  assert.equal(f.scope.invocations.size, 1);
  assert.deepEqual(calls, [["PreToolUse", "parent"]]);
  f.transport.emit({ type: "stream_event", session_id: "session", parent_tool_use_id: "parent",
    event: { type: "content_block_stop", index: 0 } });
  f.model("child", input, "Bash", "parent");
  assert.deepEqual((await f.response("child-pre")).response, {});
  const entry = f.resident.identities.active.get("child");
  assert.deepEqual(f.scope.invocations.get(entry.id).input, input);
  assert.equal(entry.parentEntry, f.resident.identities.active.get("parent"));
  f.hook("child", "child-post", "PostToolUse");
  await f.response("child-post");
  assert.deepEqual(calls, [["PreToolUse", "parent"], ["PreToolUse", "child"], ["PostToolUse", "child"]]);
  await retired(f, "child");
  await retired(f, "parent");
  await drained(f);
});

test("child native permission waits for the complete producer", { timeout: 5000 }, async t => {
  let calls = 0;
  const f = await fixture(t, undefined, { canUseTool: async () => { calls++; return { behavior: "allow" }; } });
  await parent(f);
  start(f);
  f.transport.emit({ type: "control_request", request_id: "permission", request: {
    subtype: "can_use_tool", tool_name: "Bash", input, tool_use_id: "child" } });
  await until(() => f.resident.identities.pending.has("permission"), "permission must be pending");
  await pause();
  assert.equal(calls, 0);
  assert.equal(f.transport.responses.has("permission"), false);
  f.model("child", input, "Bash", "parent");
  assert.equal((await f.response("permission")).response.behavior, "allow");
  assert.equal(calls, 1);
  await drained(f);
});

test("sibling producers resolve by exact identity in reverse arrival order", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  for (const id of ["first", "second"]) {
    start(f, id);
    f.hook(id, `${id}-pre`, "PreToolUse", { command: id });
  }
  await until(() => f.resident.identities.active.get("second")?.callbacks.size === 1, "siblings must wait");
  f.model("second", { command: "second" }, "Bash", "parent");
  await f.response("second-pre");
  assert.deepEqual(calls, ["parent", "second"]);
  assert.equal(f.transport.responses.has("first-pre"), false);
  f.model("first", { command: "first" }, "Bash", "parent");
  await f.response("first-pre");
  assert.deepEqual(calls, ["parent", "second", "first"]);
  await drained(f);
});

test("native permission replay cannot enter twice while awaiting a child producer", { timeout: 5000 }, async t => {
  let calls = 0;
  const f = await fixture(t, undefined, { canUseTool: async () => { calls++; return { behavior: "allow" }; } });
  await parent(f);
  start(f);
  for (const request of ["first", "replay"])
    f.transport.emit({ type: "control_request", request_id: request, request: {
      subtype: "can_use_tool", tool_name: "Bash", input, tool_use_id: "child" } });
  assert.equal((await f.response("replay")).response.behavior, "deny");
  assert.equal((await f.response("first")).response.behavior, "deny");
  assert.match(f.resident.failure.message, /repeated native permission/);
  assert.equal(calls, 0);
  await drained(f);
});

test("child callbacks without an announcement retain unknown identity denial", { timeout: 5000 }, async t => {
  const f = await fixture(t);
  await parent(f);
  f.hook("child", "unknown");
  denied(await f.response("unknown"));
  assert.match(f.resident.failure.message, /unknown native tool identity/);
  assert.equal(f.scope.invocations.size, 1);
  await drained(f);
});

test("child announcements require an active allowed parent Agent", { timeout: 5000 }, async t => {
  for (const state of ["missing", "issued", "denied", "wrong-tool", "retired"]) {
    const f = await fixture(t, async () => state === "denied" ? {
      hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny" },
    } : {});
    if (state === "issued") { f.model("parent", parentInput, "Agent"); await pause(); }
    else if (state !== "missing") await parent(f, "parent", state === "wrong-tool" ? "Bash" : "Agent");
    if (state === "retired") await retired(f, "parent");
    start(f);
    await until(() => f.resident.failure, "invalid parent must fence the Query");
    assert.match(f.resident.failure.message, /active admitted parent Agent/);
    assert.equal(f.resident.identities.active.has("child"), false);
    await drained(f);
  }
});

test("child producers reject changed parent name or session", { timeout: 5000 }, async t => {
  for (const field of ["parent", "name", "session"]) {
    const f = await fixture(t);
    await parent(f);
    await parent(f, "other");
    start(f);
    f.hook("child", "waiting");
    await waiting(f);
    f.transport.emit({ type: "assistant", session_id: field === "session" ? "other-session" : "session",
      parent_tool_use_id: field === "parent" ? "other" : "parent", uuid: "child-complete",
      message: { role: "assistant", content: [{ type: "tool_use", id: "child",
        name: field === "name" ? "Read" : "Bash", input }] } });
    await until(() => f.resident.failure, "changed producer must reject");
    assert.match(f.resident.failure.message, /identity/);
    assert.equal([...f.scope.invocations.values()].some(value => value.name === "Bash"), false);
    await drained(f);
  }
});

test("child Hooks reject changed input name or session before application entry", { timeout: 5000 }, async t => {
  for (const field of ["input", "name", "session"]) {
    const calls = [];
    const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
    await parent(f);
    start(f);
    f.hook("child", "tampered", "PreToolUse", field === "input" ? { command: "changed" } : input,
      { name: field === "name" ? "Read" : "Bash", session: field === "session" ? "changed" : "session" });
    if (field === "input") { await waiting(f); f.model("child", input, "Bash", "parent"); }
    denied(await f.response("tampered"));
    assert.deepEqual(calls, ["parent"]);
    await drained(f);
  }
});

test("child callback replay is reserved synchronously during producer wait", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  start(f);
  f.hook("child", "first");
  f.hook("child", "replay");
  denied(await f.response("replay"));
  denied(await f.response("first"));
  assert.match(f.resident.failure.message, /repeated/);
  assert.deepEqual(calls, ["parent"]);
  await drained(f);
});

test("duplicate child announcements and complete producers fence the Query", { timeout: 5000 }, async t => {
  for (const complete of [false, true]) {
    const f = await fixture(t);
    await parent(f);
    start(f);
    if (complete) {
      f.model("child", input, "Bash", "parent");
      f.model("child", input, "Bash", "parent");
    } else start(f);
    await until(() => f.resident.failure, "duplicate producer must fail");
    assert.match(f.resident.failure.message, /duplicate native model invocation/);
    await drained(f);
  }
});

test("pending child producers share the active invocation capacity", { timeout: 5000 }, async t => {
  const f = await fixture(t);
  f.scope.invocationLimit = 3;
  await parent(f);
  start(f, "child-1");
  start(f, "child-2");
  await until(() => f.resident.identities.active.size === 3, "pending identities consume capacity");
  assert.equal(f.scope.invocations.size, 1);
  start(f, "overflow");
  await until(() => f.resident.failure, "capacity overflow must fence the Query");
  assert.match(f.resident.failure.message, /invalid or duplicate native model invocation/);
  await drained(f);
});

test("native cancellation aborts a child producer wait and rejects late production", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  start(f);
  f.hook("child", "cancelled");
  await waiting(f);
  f.transport.emit({ type: "control_cancel_request", request_id: "cancelled" });
  await until(() => f.resident.failure, "cancellation must reach the native callback wait");
  assert.match(f.resident.failure.message, /producer wait cancelled/);
  f.model("child", input, "Bash", "parent");
  await drained(f);
  assert.deepEqual(calls, ["parent"]);
});

test("close drains child waits and unclaimed announcements", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  start(f);
  start(f, "unclaimed");
  f.hook("child", "closing");
  await waiting(f);
  await drained(f);
  assert.deepEqual(calls, ["parent"]);
  assert.throws(() => f.resident.identities.produce({ type: "assistant", session_id: "session" }), /closed/);
});

test("parent retirement rejects its waiting child producer", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  start(f);
  f.hook("child", "orphan");
  await waiting(f);
  await retired(f, "parent");
  denied(await f.response("orphan"));
  assert.match(f.resident.failure.message, /parent retired/);
  assert.deepEqual(calls, ["parent"]);
  await drained(f);
});

test("parent permission denial invalidates a waiting child", { timeout: 5000 }, async t => {
  const f = await fixture(t, undefined, { canUseTool: async () => ({ behavior: "deny", message: "native veto" }) });
  await parent(f);
  start(f);
  f.hook("child", "waiting");
  await waiting(f);
  f.transport.emit({ type: "control_request", request_id: "parent-permission", request: {
    subtype: "can_use_tool", tool_name: "Agent", input: parentInput, tool_use_id: "parent" } });
  assert.equal((await f.response("parent-permission")).response.behavior, "deny");
  f.model("child", input, "Bash", "parent");
  await until(() => f.resident.failure, "denied parent must invalidate child completion");
  assert.match(f.resident.failure.message, /active admitted parent Agent/);
  await drained(f);
});

test("parent retirement during child registration prevents callback entry", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred(), calls = [];
  t.after(() => release.resolve());
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  const register = f.scope.register;
  f.scope.register = async (...args) => { entered.resolve(); await release.promise; return register(...args); };
  start(f);
  f.hook("child", "held-admission");
  await waiting(f);
  f.model("child", input, "Bash", "parent");
  await entered.promise;
  await retired(f, "parent");
  release.resolve();
  denied(await f.response("held-admission"));
  assert.deepEqual(calls, ["parent"]);
  assert.match(f.resident.failure.message, /active admitted parent Agent/);
  await drained(f);
});

test("parent retirement during the child callback prevents its late allow", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  t.after(() => release.resolve());
  const f = await fixture(t, async (_, id) => {
    if (id === "child") { entered.resolve(); await release.promise; }
    return {};
  });
  await parent(f);
  start(f);
  f.hook("child", "late-allow");
  f.model("child", input, "Bash", "parent");
  await entered.promise;
  await retired(f, "parent");
  release.resolve();
  denied(await f.response("late-allow"));
  assert.match(f.resident.failure.message, /active admitted parent Agent/);
  await drained(f);
});

test("parent result fences new child announcements before its held queue barrier", { timeout: 5000 }, async t => {
  const f = await fixture(t);
  await parent(f);
  await heldResult(f, "parent");
  start(f);
  await until(() => f.resident.failure, "terminal parent must reject new child announcements");
  assert.match(f.resident.failure.message, /active admitted parent Agent/);
  assert.equal(f.resident.identities.active.has("child"), false);
  releaseResult(f);
  await drained(f);
});

test("parent result rejects an announced child before its held queue barrier", { timeout: 5000 }, async t => {
  const calls = [];
  const f = await fixture(t, async (_, id) => { calls.push(id); return {}; });
  await parent(f);
  start(f);
  f.hook("child", "pending-producer");
  await waiting(f);
  await heldResult(f, "parent");
  denied(await f.response("pending-producer"));
  assert.match(f.resident.failure.message, /parent retired/);
  assert.deepEqual(calls, ["parent"]);
  assert(f.resident.identities.active.has("parent"));
  releaseResult(f);
  await drained(f);
});

for (const phase of ["registration", "callback"]) {
  test(`parent result fences child ${phase} before its held queue barrier`, { timeout: 5000 }, async t => {
    const entered = deferred(), release = deferred(), calls = [];
    t.after(() => release.resolve());
    const f = await fixture(t, async (_, id) => {
      calls.push(id);
      if (phase === "callback" && id === "child") { entered.resolve(); await release.promise; }
      return {};
    });
    await parent(f);
    if (phase === "registration") {
      const register = f.scope.register;
      f.scope.register = async (...args) => { entered.resolve(); await release.promise; return register(...args); };
    }
    start(f);
    f.hook("child", "held-child");
    f.model("child", input, "Bash", "parent");
    await entered.promise;
    await heldResult(f, "parent");
    release.resolve();
    denied(await f.response("held-child"));
    assert.match(f.resident.failure.message, /active admitted parent Agent/);
    assert.deepEqual(calls, phase === "registration" ? ["parent"] : ["parent", "child"]);
    assert(f.resident.identities.active.has("parent"), "authorization must fail before queue drain");
    releaseResult(f);
    await drained(f);
  });
}

test("terminal identity admits late Post and retains its original finalizer through queue drain", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  let finalized = false;
  t.after(() => release.resolve());
  const f = await fixture(t, async payload => {
    if (payload.hook_event_name === "PostToolUse") {
      entered.resolve();
      try { await release.promise; } finally { finalized = true; }
    }
    return {};
  });
  await parent(f);
  await heldResult(f, "parent");
  f.hook("parent", "late-post", "PostToolUse", parentInput, { name: "Agent" });
  await entered.promise;
  releaseResult(f);
  await until(() => f.resident.identities.active.get("parent")?.phase === "retiring", "queue barrier must finish");
  assert.equal(finalized, false);
  assert.equal(f.scope.invocations.size, 1, "original Post finalizer retains its registered identity");
  release.resolve();
  assert.deepEqual((await f.response("late-post")).response, {});
  await until(() => !f.resident.identities.active.has("parent"), "Post finalizer must precede retirement");
  assert.equal(finalized, true);
  assert.equal(f.resident.failure, null);
  await drained(f);
});

test("terminal parent allows an authorized child's late Post and finalizer drain", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  let finalized = false;
  t.after(() => release.resolve());
  const f = await fixture(t, async payload => {
    if (payload.hook_event_name === "PostToolUse") {
      entered.resolve();
      try { await release.promise; } finally { finalized = true; }
    }
    return {};
  });
  await parent(f);
  start(f);
  f.hook("child", "child-pre");
  f.model("child", input, "Bash", "parent");
  await f.response("child-pre");
  await heldResult(f, "parent");
  f.hook("child", "late-child-post", "PostToolUse");
  await entered.promise;
  releaseResult(f);
  await until(() => !f.resident.identities.active.has("parent"), "parent queue barrier must finish");
  f.transport.emit({ type: "user", message: { content: [{ type: "tool_result", tool_use_id: "child" }] } });
  await until(() => f.resident.identities.active.get("child")?.phase === "retiring", "child queue barrier must finish");
  assert.equal(finalized, false);
  assert.equal(f.scope.invocations.size, 1, "child Post finalizer retains its own registered identity");
  release.resolve();
  assert.deepEqual((await f.response("late-child-post")).response, {});
  await until(() => !f.resident.identities.active.has("child"), "child Post finalizer must precede retirement");
  assert.equal(finalized, true);
  assert.equal(f.resident.failure, null);
  await drained(f);
});

test("child producer deadline fences and drains callbacks and announcements", { timeout: 15000 }, async t => {
  const f = await fixture(t);
  await parent(f);
  start(f);
  start(f, "unclaimed");
  f.hook("child", "timeout");
  await waiting(f);
  await new Promise(resolve => setTimeout(resolve, 12050));
  denied(await f.response("timeout"));
  assert.match(f.resident.failure.message, /child producer deadline exceeded/);
  await drained(f);
});

test("long child producer sequences reclaim bounded identity and callback state", { timeout: 15000 }, async t => {
  const f = await fixture(t);
  f.scope.invocationLimit = 2;
  for (let sequence = 0; sequence < 1280; sequence++) {
    const parentId = `parent-${sequence}`, id = `child-${sequence}`;
    await parent(f, parentId);
    start(f, id, { parent: parentId });
    f.hook(id, `request-${sequence}`);
    await until(() => f.resident.identities.active.get(id)?.callbacks.size === 1, "child must wait");
    f.model(id, input, "Bash", parentId);
    assert.deepEqual((await f.response(`request-${sequence}`)).response, {});
    await retired(f, id);
    await retired(f, parentId);
    assert.equal(f.scope.invocations.size, 0);
    assert.equal(f.resident.identities.active.size, 0);
    assert.equal(f.resident.identities.pending.size, 0);
    assert.equal(f.resident.callbacks.size, 0);
  }
  f.hook("child-0", "retired-child");
  denied(await f.response("retired-child"));
  await drained(f);
});
