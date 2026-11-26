import assert from "node:assert/strict";
import test from "node:test";

import { runProfileCall } from "../src/profile-call.mjs";
import { field, hook, target } from "./profile-fixture.mjs";

function call(
  input,
  codec,
  dispatch,
  execute,
  { withPrompt = false, path = "arg.0.payload" } = {},
) {
  const hooks = { "tool.before": hook(["block"]) };
  if (withPrompt) hooks["input.submit"] = hook();
  return runProfileCall({
    target: target("run", {
      call: "async",
      hooks,
      fields: {
        ...(withPrompt && { prompt: field("arg.0.prompt") }),
        tool_name: { const: "tool" },
        input: field(path, codec),
      },
    }),
    args: [input],
    options: {},
    binding: {
      sessionId: "session",
      routes: { hooks: Object.keys(hooks).map((event) => ({ event })) },
      dispatch,
    },
    callNative: ([argument]) => execute(argument),
    release() {},
  });
}

test("native async work receives an isolated copy of the admitted JSON field", async () => {
  const entered = Promise.withResolvers();
  const resume = Promise.withResolvers();
  const other = Symbol("native state");
  const input = { other, payload: { item: "approved" } };
  const result = call(
    input,
    "json",
    ([, , checked]) => {
      assert.deepEqual(checked, { item: "approved" });
      return { decision: {} };
    },
    async (argument) => {
      assert.equal(argument.other, other);
      entered.resolve();
      await resume.promise;
      return argument.payload.item;
    },
  );
  await entered.promise;
  input.payload.item = "later";
  resume.resolve();
  assert.equal(await result, "approved");
});

test("a text projection keeps the admitted native type through dispatch", async () => {
  const input = { payload: 1 };
  const result = call(
    input,
    "text",
    () => {
      input.payload = "1";
      return { decision: {} };
    },
    () => assert.fail("native work must wait for matching input"),
  );
  await assert.rejects(result, { code: "source_input_mutated" });
});

test("JSON field snapshots only read plain data descriptors", async () => {
  const accessed = () => assert.fail("snapshot invoked application code");
  for (const item of [
    {
      get value() {
        return accessed();
      },
    },
    new Proxy({}, { ownKeys: accessed }),
  ]) {
    await assert.rejects(
      call({ payload: { item } }, "json", accessed, accessed),
      { code: "source_invalid_payload" },
    );
  }
});

test("nested text and encoded JSON retain their admitted native representation", async () => {
  for (const [codec, original] of [
    ["text", 42n],
    ["json_string", ' { "value": "first" } '],
  ]) {
    const entered = Promise.withResolvers();
    const resume = Promise.withResolvers();
    const input = { payload: original };
    const result = call(
      input,
      codec,
      () => ({ decision: {} }),
      async (argument) => {
        entered.resolve();
        await resume.promise;
        return argument.payload;
      },
    );
    await entered.promise;
    input.payload = "later";
    resume.resolve();
    assert.equal(await result, original);
  }
});

test("prompt admission covers subsequent Hook waits and native async reads", async () => {
  const input = { prompt: "first", payload: { value: 1 } };
  const result = call(
    input,
    "json",
    ([operation]) => {
      if (operation === "beforeTool") input.prompt = "changed";
      return { decision: {} };
    },
    () => assert.fail("native code ran with a changed prompt"),
    { withPrompt: true },
  );
  await assert.rejects(result, { code: "source_input_mutated" });
  input.prompt = "first";
  const entered = Promise.withResolvers();
  const resume = Promise.withResolvers();
  const read = call(
    input,
    "json",
    () => ({ decision: {} }),
    async (argument) => {
      entered.resolve();
      await resume.promise;
      return argument.prompt;
    },
    { withPrompt: true },
  );
  await entered.promise;
  input.prompt = "later";
  resume.resolve();
  assert.equal(await read, "first");
});

test("admitted JSON dictionaries keep their null prototype at native entry", async () => {
  const dict = Object.assign(Object.create(null), { key: "value" });
  const output = await call(
    { payload: dict },
    "json",
    () => ({ decision: {} }),
    (arg) => arg.payload,
  );
  assert.notEqual(output, dict);
  assert.deepEqual(output, dict);
  assert.equal(Object.getPrototypeOf(output), null);
});

test("rebuilding a mapped array path retains the array prototype and data properties", async () => {
  let constructed = 0;
  class Values extends Array {
    constructor(...args) {
      super(...args);
      constructed++;
    }
    label() {
      return this[0];
    }
  }
  const values = new Values(3);
  values[2] = "last";
  Object.defineProperty(values, "0", {
    value: "approved",
    enumerable: false,
  });
  const copy = await call(
    values,
    "text",
    () => ({ decision: {} }),
    (argument) => argument,
    { path: "arg.0.0" },
  );
  assert.notEqual(copy, values);
  assert.equal(Array.isArray(copy), true);
  assert.equal(Object.getPrototypeOf(copy), Values.prototype);
  assert.equal(copy.label(), "approved");
  assert.deepEqual(
    Object.getOwnPropertyDescriptors(copy),
    Object.getOwnPropertyDescriptors(values),
  );
  assert.equal(constructed, 1);
});
