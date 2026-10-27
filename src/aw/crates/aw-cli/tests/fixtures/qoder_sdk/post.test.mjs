/** Strict Post publication waits for native shutdown and original callback cleanup. */
import test from "node:test";
import assert from "node:assert/strict";
import { Resident } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";
import { postTools } from "../../../../aw-adapter/native/qoder-sdk/post.mjs";

const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

function fixture(action = "withhold_result") {
  const resident = new Resident({ hooks: [{ event: "tool.after", step: "post",
    on_error: action, budget_ms: 1000 }] });
  const shutdown = deferred();
  const state = { interrupts: 0, closes: 0, finished: false, result: undefined };
  resident.query = { interrupt: async () => { state.interrupts++; } };
  resident.nativeClose = async () => { state.closes++; await shutdown.promise; };
  resident.scope.handle = {};
  resident.scope.running = true;
  resident.scope.drain = async () => { state.finished = true; };
  resident.scope.close = async () => {};
  const payload = { hook_event_name: "PostToolUse", tool_response: { text: "original" } };
  return { resident, shutdown, state, payload };
}

for (const failure of ["exit", "timeout", "invalid"]) {
  test(`strict Post ${failure} waits for shutdown and fences dispatch`, async () => {
    const f = fixture();
    f.resident.scope.invoke = async () => {
      if (failure === "timeout") throw new Error("callback_timeout");
      return { code: failure === "exit" ? 1 : 0,
        stdout: Buffer.from(failure === "invalid" ? '{"unexpected":true}' : "") };
    };
    let settled = false;
    const pending = postTools(f.resident, f.payload).then(
      () => { settled = true; }, error => { settled = true; return error; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(f.state.interrupts, 1);
    assert.equal(f.state.closes, 1);
    assert.equal(settled, false);
    assert.throws(() => f.resident.scope.checkExecution(), /closed/);
    f.shutdown.resolve();
    const error = await pending;
    assert(error instanceof Error);
    assert.equal(error, f.resident.failure);
    await f.resident.close();
    assert.equal(f.state.closes, 1);
    assert.equal(f.state.finished, true);
  });
}

test("report failure preserves the original success and native error envelope", async () => {
  for (const payload of [{ hook_event_name: "PostToolUse", tool_response: { content: [{ type: "text", text: "native" }] } },
    { hook_event_name: "PostToolUseFailure", error: "native exit 7", is_interrupt: false }]) {
    const f = fixture("report");
    const original = structuredClone(payload);
    f.resident.scope.invoke = async () => { throw new Error("reported provider failure"); };
    await postTools(f.resident, payload);
    assert.deepEqual(payload, original);
    assert.equal(f.resident.failure, null);
    assert.equal(f.state.interrupts, 0);
    assert.equal(f.state.closes, 0);
    f.resident.scope.checkExecution();
  }
});

test("strict Post success leaves the native payload intact", async () => {
  const f = fixture();
  f.resident.scope.invoke = async () => ({ code: 0, stdout: Buffer.from("{}") });
  const original = structuredClone(f.payload);
  await postTools(f.resident, f.payload);
  assert.deepEqual(f.payload, original);
  assert.equal(f.resident.failure, null);
  assert.equal(f.state.closes, 0);
});

test("resident close joins original callbacks after strict Post closes the native waiter", async () => {
  const f = fixture();
  const finallyGate = deferred();
  const callback = f.resident.track(async () => {
    try { await f.shutdown.promise; }
    finally { await finallyGate.promise; }
  });
  const failure = new Error("Post failure");
  const withholding = f.resident.withhold(failure).catch(error => error);
  f.shutdown.resolve();
  assert.equal(await withholding, failure);
  let closed = false;
  const closing = f.resident.close().then(() => { closed = true; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(closed, false);
  assert.equal(f.state.finished, false);
  finallyGate.resolve();
  await Promise.all([callback, closing]);
  assert.equal(f.state.finished, true);
  assert.equal(f.resident.callbacks.size, 0);
});

test("native shutdown failure retains scope ownership", async () => {
  const f = fixture();
  f.resident.nativeClose = async () => { throw new Error("worker shutdown failed"); };
  await assert.rejects(f.resident.withhold(new Error("Post rejected")), /withholding cleanup/);
  await assert.rejects(f.resident.close(), /scope retained/);
  assert.equal(f.state.finished, false);
  assert(f.resident.scope.handle);
});

for (const invalidIdentity of [false, true]) {
  test(`strict Post ${invalidIdentity ? "unknown identity" : "application failure"} closes before replying`, async () => {
    const f = fixture();
    let entered = false;
    if (!invalidIdentity) f.resident.identities.claim = async () => ({ phase: "allowed" });
    const hooks = f.resident.decorate({ hooks: { PostToolUse: [{ hooks: [async () => {
      entered = true;
      throw new Error("application Post failure");
    }] }] } }).hooks;
    let settled = false;
    const pending = hooks.PostToolUse[0].hooks[0]({}, "native", {})
      .catch(error => { settled = true; return error; });
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(entered, !invalidIdentity);
    assert.equal(settled, false);
    assert.equal(f.state.closes, 1);
    f.shutdown.resolve();
    assert.equal(await pending, f.resident.failure);
    await f.resident.close();
  });
}
