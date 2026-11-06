/** Prewarming retains native one-shot input and the resident cleanup owner. */
import assert from "node:assert/strict";
import test from "node:test";
import { prewarm } from "../../../../aw-adapter/native/qoder-sdk/startup.mjs";
import { fixture } from "./resident-support.mjs";

test("prewarm initializes without a prompt and releases exactly one native turn", async () => {
  let source, checked = 0, closes = 0;
  const session = { async close() { closes++; } };
  const options = { settingSources: ["user", "project", "local"], settings: "relative.json" };
  const warm = await prewarm(async (input, lifecycle) => {
    assert.equal(input.options, options);
    lifecycle.check = () => { checked++; };
    source = input.prompt;
    return session;
  }, { options });
  let consumed = false;
  const next = source.next().then(value => { consumed = true; return value; });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(consumed, false);
  assert.equal(warm.query("hello"), session);
  assert.equal(checked, 1);
  assert.deepEqual((await next).value.message.content, [{ type: "text", text: "hello" }]);
  assert.throws(() => warm.query("again"), /once/);
  await Promise.all([warm.close(), warm[Symbol.asyncDispose]()]);
  assert.equal(closes, 1);
});

test("unused prewarm closes the input gate and rejects later activation", async () => {
  let source;
  const warm = await prewarm(async input => {
    source = input.prompt;
    return { close: async () => { assert.equal((await source.next()).done, true); } };
  });
  await warm.close();
  assert.throws(() => warm.query("late"), /closed/);
});

test("prewarm timeout discards input before joining native cleanup", { timeout: 5000 }, async t => {
  const f = fixture(t, true);
  f.state.importGate.resolve();
  f.state.closeGate.resolve();
  const nativeClose = f.state.query.close;
  f.state.query.close = async () => {
    await f.state.dispatch;
    await nativeClose();
  };
  await assert.rejects(prewarm((input, lifecycle) => f.resident.create(input.prompt,
    { cwd: process.cwd() }, (_config, options) => ({ ...options, hooks: {} }),
    () => { throw new Error("must not become ready"); }, lifecycle), { initializeTimeoutMs: 10 }), /deadline/);
  assert.equal(f.state.executions, 0);
  assert.deepEqual(f.state.events, ["native-closed", "scope-closed"]);
});

test("prewarm checks retained ownership before publishing its prompt", async () => {
  let input;
  const warm = await prewarm(async (request, lifecycle) => {
    input = request.prompt;
    lifecycle.check = () => { throw new Error("expired owner"); };
    return { close: async () => {} };
  });
  assert.throws(() => warm.query("never submit"), /expired owner/);
  await warm.close();
  assert.equal((await input.next()).done, true);
});
