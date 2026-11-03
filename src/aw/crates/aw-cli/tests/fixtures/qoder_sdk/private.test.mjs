/** Explicit consumers retain their credentials until raw work and finally drain. */
import test from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";

let sequence = 0;
const moduleUrl = new URL("../../../../aw-adapter/native/qoder-sdk/private.mjs", import.meta.url);
const load = () => import(`${moduleUrl}?private-case=${++sequence}`);
function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}
const invocation = (id = "call") => ({ invocationId: id, toolName: "fixture",
  input: { command: "approved", nested: ["unchanged"] } });
function lease(api, secret, release = async () => {}) {
  return { version: api.PRIVATE_EXECUTION_VERSION, environment: {}, hookEnvironment: {},
    secrets: { AW_FIXTURE_SECRET: secret },
    redact: value => JSON.parse(JSON.stringify(value).replaceAll(secret, "[redacted]")), release };
}

test("private consumers isolate concurrent environments and preserve scan arguments", { timeout: 2000 }, async () => {
  const api = await load();
  const entered = deferred(), finish = deferred();
  const releases = [];
  const owner = new api.ExecutionOwner(async context => {
    assert.throws(() => { context.input.command = "changed"; }, TypeError);
    assert.throws(() => { context.input.nested.push("changed"); }, TypeError);
    return lease(api, `secret-${context.invocationId}`, async () => releases.push(context.invocationId));
  });
  let count = 0;
  const consume = (context) => owner.run(context, async approved => {
    if (++count === 2) entered.resolve();
    await finish.promise;
    assert.equal(process.env.AW_FIXTURE_SECRET, undefined);
    assert.deepEqual(api.hookEnvironment(), {});
    const environment = api.executionEnvironment();
    const text = execFileSync(process.execPath, ["-e", "process.stdout.write(process.env.AW_FIXTURE_SECRET)"],
      { env: environment, encoding: "utf8", timeout: 1000 });
    environment.AW_FIXTURE_SECRET = "consumer-copy";
    assert.equal(api.executionEnvironment().AW_FIXTURE_SECRET, `secret-${approved.invocationId}`);
    return { content: [{ type: "text", text }], isError: false, structuredContent: { id: approved.invocationId } };
  });
  const original = invocation("a");
  const a = consume(original), b = consume(invocation("b"));
  await entered.promise;
  finish.resolve();
  const results = await Promise.all([a, b]);
  assert.deepEqual(original, invocation("a"));
  assert(results.every(result => result.content[0].text === "[redacted]" && result.isError === false));
  assert.deepEqual(releases.sort(), ["a", "b"]);
  assert.throws(() => api.executionEnvironment(), /no active/);
  await owner.close();
});

test("factory failure prevents tool execution and fences the owner", async () => {
  const api = await load();
  let executed = false;
  const owner = new api.ExecutionOwner(async () => { throw new Error("fixture-secret"); });
  await assert.rejects(owner.run(invocation(), () => { executed = true; }), error => {
    assert(!String(error).includes("fixture-secret"));
    assert.equal(error.cause, undefined);
    return true;
  });
  assert.equal(executed, false);
  assert.throws(() => owner.check(), /failed/);
  await owner.close();
});

test("invalid lease versions and secret Hook views release without dispatch", async () => {
  for (const change of [value => { value.version = "unsupported"; },
    value => { value.hookEnvironment = { SAFE: "contains-fixture-secret" }; },
    value => { value.hookEnvironment = { AW_FIXTURE_SECRET: "different" }; }]) {
    const api = await load();
    let released = false, executed = false;
    const owner = new api.ExecutionOwner(async () => {
      const value = lease(api, "fixture-secret", async () => { released = true; });
      change(value);
      return value;
    });
    await assert.rejects(owner.run(invocation(), () => { executed = true; }), /withheld/);
    assert.equal(executed, false);
    assert.equal(released, true);
    await owner.close();
  }
});

test("close waits for a late factory and releases its lease without dispatch", { timeout: 2000 }, async () => {
  const api = await load();
  const entered = deferred(), finish = deferred();
  let released = false, executed = false, closed = false;
  const owner = new api.ExecutionOwner(async () => {
    entered.resolve();
    await finish.promise;
    return lease(api, "fixture-secret", async () => { released = true; });
  });
  const operation = assert.rejects(owner.run(invocation(), () => { executed = true; }), /withheld/);
  await entered.promise;
  const closing = owner.close().then(() => { closed = true; });
  await Promise.resolve();
  assert.equal(closed, false);
  assert.equal(released, false);
  finish.resolve();
  await Promise.all([operation, closing]);
  assert.equal(released, true);
  assert.equal(executed, false);
});

test("cancellation retains credentials through the original tool finally", { timeout: 2000 }, async () => {
  const api = await load();
  const entered = deferred(), finalizing = deferred(), finish = deferred();
  let released = false;
  const owner = new api.ExecutionOwner(async () => lease(api, "fixture-secret", async () => {
    released = true;
  }));
  const operation = assert.rejects(owner.run(invocation(), async (_, { signal }) => {
    try {
      entered.resolve();
      await new Promise(resolve => signal.addEventListener("abort", resolve, { once: true }));
    } finally {
      finalizing.resolve();
      await finish.promise;
      assert.equal(api.executionEnvironment().AW_FIXTURE_SECRET, "fixture-secret");
      assert.equal(released, false);
    }
    return {};
  }), /withheld/);
  await entered.promise;
  const closing = owner.close();
  await finalizing.promise;
  assert.equal(released, false);
  finish.resolve();
  await Promise.all([operation, closing]);
  assert.equal(released, true);
});

test("cleanup timeout retains raw work and fences newly created owners", { timeout: 2000 }, async () => {
  const api = await load();
  const entered = deferred(), finish = deferred();
  let released = false;
  const owner = new api.ExecutionOwner(async () => lease(api, "fixture-secret", async () => {
    released = true;
  }), { cleanupTimeoutMs: 20 });
  const operation = assert.rejects(owner.run(invocation(), async () => {
    entered.resolve();
    await finish.promise;
    assert.equal(api.executionEnvironment().AW_FIXTURE_SECRET, "fixture-secret");
    return {};
  }), /deadline/);
  await entered.promise;
  await assert.rejects(owner.close(), /deadline/);
  assert.equal(released, false);
  assert.throws(() => new api.ExecutionOwner(async () => {}).check(), /restart/);
  finish.resolve();
  await operation;
  assert.equal(released, true);
  await assert.rejects(owner.close(), /deadline/);
});

test("release failure fences the process and suppresses credential diagnostics", async () => {
  const api = await load();
  const owner = new api.ExecutionOwner(async () => lease(api, "fixture-secret", async () => {
    throw new Error("fixture-secret");
  }));
  await assert.rejects(owner.run(invocation(), async () => ({})), /release failed/);
  assert.throws(() => new api.ExecutionOwner(async () => {}).check(), /restart/);
  await assert.rejects(owner.close(), /release failed/);
});

test("tool exceptions and invalid redactors cannot deliver secret diagnostics", async () => {
  for (const invalidRedactor of [false, true]) {
    const api = await load();
    let released = false;
    const owner = new api.ExecutionOwner(async () => {
      const value = lease(api, "fixture-secret", async () => { released = true; });
      if (invalidRedactor) value.redact = async value => value;
      return value;
    });
    await assert.rejects(owner.run(invocation(), async () => {
      if (!invalidRedactor) throw new Error("fixture-secret");
      return { content: [{ type: "text", text: "fixture-secret" }] };
    }), error => !String(error).includes("fixture-secret"));
    assert.equal(released, true);
    await owner.close();
  }
});

test("already cancelled work never enters a factory", async () => {
  const api = await load();
  let acquired = false;
  const owner = new api.ExecutionOwner(async () => { acquired = true; });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(owner.run(invocation(), async () => ({}), { signal: controller.signal }), /withheld/);
  assert.equal(acquired, false);
  await owner.close();
});
