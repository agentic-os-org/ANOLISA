/** Exercise the production model identity seam without an optional SDK install. */
import assert from "node:assert/strict";
import test from "node:test";
import { modelProducer } from "../../crates/aw-adapter/native/pi/producer.mjs";

const scope = () => ({ execution: 1, checkExecution(generation = this.execution) {
  if (this.failed || generation !== this.execution) throw new Error("closed execution");
}, invocations: new Map(), invocationLimit: 2, failed: false });
const stream = async () => {
  const message = { content: [{ type: "toolCall", id: "repeated", name: "tool", arguments: { n: 1 } }], stopReason: "toolUse" };
  return { async result() { return message; }, async *[Symbol.asyncIterator]() { yield { type: "done", message }; } };
};

test("repeated native IDs produce fresh identities over 1280 retirements", async () => {
  const producer = modelProducer(scope());
  let previous;
  for (let i = 0; i < 1280; i++) {
    const response = await producer.wrap(stream)();
    const [first, second] = await Promise.all([response.result(), response.result()]);
    assert.equal(first, second);
    const call = first.content[0];
    assert.notEqual(call.id, "repeated");
    assert.notEqual(call.id, previous);
    for await (const event of response) assert.equal(event.message.content[0], call);
    producer.claim({ toolCall: call });
    assert.throws(() => producer.claim({ toolCall: call }));
    previous = call.id;
  }
});

test("copied or changed tool objects cannot claim the original", async () => {
  const producer = modelProducer(scope());
  const call = (await (await producer.wrap(stream)()).result()).content[0];
  assert.throws(() => producer.claim({ toolCall: structuredClone(call) }));
  call.name = "changed";
  assert.throws(() => producer.claim({ toolCall: call }));
  call.name = "tool";
  call.arguments.n = 2;
  assert.throws(() => producer.claim({ toolCall: call }));
});

test("pending and active calls share capacity and latch overload", async () => {
  const owner = scope();
  owner.invocations.set("slow", 1);
  const producer = modelProducer(owner);
  await (await producer.wrap(stream)()).result();
  await assert.rejects(() => producer.wrap(stream)().then(response => response.result()));
  assert.equal(owner.failed, true);
});

test("execution drain rejects a late call even when native IDs repeat", async () => {
  const producer = modelProducer(scope());
  const old = (await (await producer.wrap(stream)()).result()).content[0];
  producer.clear();
  const fresh = (await (await producer.wrap(stream)()).result()).content[0];
  assert.throws(() => producer.claim({ toolCall: old }));
  producer.claim({ toolCall: fresh });
});

test("a delayed result cannot publish calls after execution changes", async () => {
  const owner = scope();
  const producer = modelProducer(owner);
  const response = await producer.wrap(stream)();
  owner.execution++;
  await assert.rejects(response.result(), /closed execution/);
  assert(owner.failed);
});

test("cancelled execution rejects a delayed result before exposing tools", async () => {
  const owner = scope();
  const response = await modelProducer(owner).wrap(stream)();
  owner.failed = true;
  await assert.rejects(response.result(), /closed execution/);
});
