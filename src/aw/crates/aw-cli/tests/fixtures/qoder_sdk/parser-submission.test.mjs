/** Native non-tool callbacks retain their cancellation ownership without tool claims. */
import test from "node:test";
import assert from "node:assert/strict";
import { fixture, deferred, pause } from "./parser-support.mjs";

function submit(f, request) {
  const callback = f.transport.hooks.UserPromptSubmit[0].hookCallbackIds[0];
  f.transport.emit({ type: "control_request", request_id: request,
    request: { subtype: "hook_callback", callback_id: callback,
      input: { hook_event_name: "UserPromptSubmit", session_id: "session",
        cwd: process.cwd(), prompt: "fresh input" } } });
}

test("native submit block survives SDK parsing without a tool claim", { timeout: 5000 }, async t => {
  const f = await fixture(t, undefined, { hooks: { UserPromptSubmit: [{ hooks: [async (input, id) => {
    assert.equal(id, undefined);
    assert.equal(input.prompt, "fresh input");
    return { decision: "block", reason: "AW input risk" };
  }] }] } });
  submit(f, "submit");
  const result = await f.response("submit");
  assert.equal(result.subtype, "success");
  assert.deepEqual(result.response, { decision: "block", reason: "AW input risk" });
  assert.equal(f.scope.invocations.size, 0);
  assert.equal(f.resident.failure, null);
});

test("native submit cancellation retains its original callback until cleanup", { timeout: 5000 }, async t => {
  const entered = deferred(), release = deferred();
  let drained = false;
  const f = await fixture(t, undefined, { hooks: { UserPromptSubmit: [{ hooks: [async () => {
    entered.resolve();
    try { await release.promise; } finally { drained = true; }
    return {};
  }] }] } });
  submit(f, "held-submit");
  await entered.promise;
  f.transport.emit({ type: "control_cancel_request", request_id: "held-submit" });
  await pause();
  let closed = false;
  const close = f.resident.close().then(() => { closed = true; });
  await pause();
  assert.equal(closed, false);
  assert.equal(drained, false);
  release.resolve();
  await close;
  assert.equal(drained, true);
  assert.equal(f.scope.invocations.size, 0);
});
