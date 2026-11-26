import assert from "node:assert/strict";
import { AsyncLocalStorage } from "node:async_hooks";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import { execFile } from "node:child_process";
import { join } from "node:path";
import { promisify } from "node:util";
import { setTimeout as delay } from "node:timers/promises";
import { ProfileSource } from "../src/profile-source.mjs";
import { SourceDenied, SourceResultWithheld } from "../src/sources.mjs";
import {
  appFixture,
  field,
  hook,
  installProfile,
  policyFixture,
  profile,
  target,
} from "./profile-fixture.mjs";
import {
  BINARY,
  readProviderLog,
  startAwService,
} from "./source-mutation-fixture.mjs";

// Rust's SourceCapability round-trip test checks these same admission cases.
const { vectors } = JSON.parse(
  readFileSync(
    new URL(
      "../../../crates/aw-contracts/tests/fixtures/source-plan-vectors.json",
      import.meta.url,
    ),
  ),
);
const { hooks } = vectors.find(
  ({ name }) => name === "native-qwenpaw",
).document;
const toolHooks = {
  "tool.before": hooks["tool.before"],
  "tool.after": hooks["tool.after"],
};
const inputHook = { "input.submit": hooks["input.submit"] };
const tools = (
  codec = "json",
  from = "arg.0.payload",
  resultCodec = "text",
) => ({
  tool_name: { const: "profile-tool" },
  input: field(from, codec),
  result: field("result", resultCodec),
  session_id: field("arg.0.session"),
  call_id: field("arg.0.call"),
});
const toolRoutes = [
  {
    event: "tool.before",
    step: "before",
    on_error: "block",
    replace_input: true,
    budget_ms: 1000,
  },
  {
    event: "tool.after",
    step: "after",
    on_error: "withhold_result",
    replace_result: true,
    budget_ms: 1000,
  },
];
const bounded = { buffer: true, max_items: 2, max_bytes: 60 };
const readAsync = async (iterable) => {
  const items = [];
  for await (const item of iterable) items.push(item);
  return items;
};

test(
  "four call shapes share a Scope and enforce rewrites, withholding, and bounded delivery",
  { timeout: 15000 },
  async (t) => {
    const { root, entry } = appFixture(t, {
      "main.cjs": `
      exports.sync = ({ payload }) => payload.command;
      exports.json = ({ payload }) => JSON.parse(payload).command;
      exports.async = async ({ payload }) => payload.command;
      exports.syncChunks = function* ({ payload }) { yield payload.command; yield "last"; return "done"; };
      exports.asyncChunks = async function* ({ payload }) { yield payload.command; yield "last"; };
      exports.many = function* () { yield "a"; yield "b"; yield "c"; };
      exports.big = async function* () { yield "x".repeat(60); };
    `,
    });
    const service = await policyFixture(t, root, toolRoutes);
    const source = await installProfile(
      t,
      profile([
        target("sync", { hooks: toolHooks, fields: tools() }),
        target("json", { hooks: toolHooks, fields: tools("json_string") }),
        target("async", { hooks: toolHooks, fields: tools(), call: "async" }),
        ...["syncChunks", "asyncChunks", "many", "big"].map((name, i) =>
          target(name, {
            hooks: toolHooks,
            fields: tools("json", "arg.0.payload", "json"),
            call: i % 2 ? "async_iter" : "sync_iter",
            stream: bounded,
          }),
        ),
      ]),
      { resolveFrom: entry },
    );
    const sdk = createRequire(entry)("profile-fixture");
    const cached = sdk.sync;
    assert.equal(cached({ payload: { command: "raw:before" } }), "raw:before");
    assert.equal(source.activation_epoch, 0);
    assert.deepEqual(service.events(), []);

    await source.activate({
      endpoint: service.endpoint,
      target: "fixture",
      sessionId: "default",
    });
    assert.equal(source.activation_epoch, 1);
    const input = {
      payload: { command: "raw:sync" },
      session: "session-sync",
      call: "call-sync",
    };
    assert.equal(sdk.sync(input), "safe:sync");
    assert.equal(input.payload.command, "raw:sync");
    const json = { payload: '{"command":"raw:json"}' };
    assert.equal(sdk.json(json), "safe:json");
    assert.equal(json.payload, '{"command":"raw:json"}');
    assert.equal(await sdk.async({ payload: { command: "replace" } }), "clean");
    assert.throws(
      () => sdk.sync({ payload: { command: "withhold" }, session: "private" }),
      (error) =>
        error instanceof SourceResultWithheld &&
        error.sessionId === "private" &&
        error.toolExecuted,
    );
    assert.throws(
      () => sdk.sync({ payload: { command: "blocked" } }),
      SourceDenied,
    );
    const stream = sdk.syncChunks({ payload: { command: "replace" } });
    assert.deepEqual(stream.next(), { done: false, value: "clean" });
    assert.deepEqual(stream.next(), { done: true, value: "done" });
    assert.deepEqual(
      await readAsync(sdk.asyncChunks({ payload: { command: "raw:stream" } })),
      ["safe:stream", "last"],
    );
    assert.throws(() => sdk.many({ payload: {} }).next(), {
      code: "source_stream_result_too_large",
    });
    await assert.rejects(sdk.big({ payload: {} }).next(), {
      code: "source_stream_result_too_large",
    });
    const seen = service.events();
    assert.deepEqual(
      seen
        .slice(0, 2)
        .map(({ event, call_id, session_id, input }) => [
          event,
          call_id,
          session_id,
          input.command,
        ]),
      [
        ["tool.before", "call-sync", "session-sync", "raw:sync"],
        ["tool.after", "call-sync", "session-sync", "safe:sync"],
      ],
    );
    assert.equal(
      seen.find(
        ({ event, input }) =>
          event === "tool.before" && input.command === "raw:json",
      ).session_id,
      "default",
    );
    await source.teardown();
    assert.throws(() => cached({ payload: {} }), {
      code: "source_unavailable",
    });
  },
);

test("an unrouted tool.after passes native chunks through without collection or read-ahead", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.reads = 0;
      function* chunks(item) { for (let i = 0; i < 3; i++) { exports.reads++; yield item; } }
      exports.sync = chunks;
      exports.async = async function* (item) { yield* chunks(item); };`,
  });
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const hooks = {
    "session.start": hook(),
    "tool.after": toolHooks["tool.after"],
  };
  const fields = {
    tool_name: { const: "chunks" },
    result: field("result", "json"),
  };
  const source = await installProfile(
    t,
    profile([
      target("sync", { call: "sync_iter", hooks, fields, stream: bounded }),
      target("async", { call: "async_iter", hooks, fields, stream: bounded }),
    ]),
    { resolveFrom: entry },
  );
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const sdk = createRequire(entry)("profile-fixture");
  const native = Symbol("opaque native chunk");
  for (const method of ["sync", "async"]) {
    const reads = sdk.reads;
    const stream = sdk[method](native);
    for (let i = 1; i <= 3; i++) {
      assert.deepEqual(await stream.next(), { done: false, value: native });
      assert.equal(sdk.reads, reads + i);
    }
    assert.equal((await stream.next()).done, true);
  }
  await source.teardown();
  assert.deepEqual(
    service.events().map(({ event }) => event),
    ["session.start", "session.start"],
  );
});

test("concurrent method calls retain extracted, resolved, and AsyncLocalStorage session and turn identities", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.Agent = class Agent {
      constructor(id) { this.id = id; }
      async ask({ prompt, wait }) { await wait; return prompt; }
    };`,
  });
  const hooks = {
    "session.start": hook(),
    ...inputHook,
    "turn.stop": hook(["block"], ["block"]),
    "session.end": hook(),
  };
  const routes = Object.keys(hooks).map((event) => ({
    event,
    step: "record",
    on_error: hooks[event].failure_actions[0] ?? "report",
    budget_ms: 1000,
  }));
  const service = await policyFixture(t, root, routes);
  const sessionStorage = new AsyncLocalStorage();
  const source = await installProfile(
    t,
    profile([
      target("Agent.prototype.ask", {
        call: "async",
        hooks,
        fields: {
          session_id: field("receiver.id"),
          turn_id: field("arg.0.turn"),
          prompt: field("arg.0.prompt"),
        },
      }),
    ]),
    {
      resolveFrom: entry,
      sessionStorage,
      resolveSessionId: ({ args }) => args[0].resolve,
    },
  );
  const { Agent } = createRequire(entry)("profile-fixture");
  const main = await source.activate({
    endpoint: service.endpoint,
    target: "fixture",
    sessionId: "default",
  });
  const first = Promise.withResolvers();
  const a = sessionStorage.run("als-unused", () =>
    new Agent("extracted").ask({
      turn: "t1",
      prompt: "first",
      wait: first.promise,
    }),
  );
  const b = sessionStorage.run({ sessionId: "als" }, () =>
    new Agent(null).ask({ turn: "t2", prompt: "second" }),
  );
  const c = new Agent().ask({
    turn: "t3",
    prompt: "third",
    resolve: "resolved",
  });
  assert.equal(await b, "second");
  assert.equal(await c, "third");
  first.resolve();
  assert.equal(await a, "first");
  await assert.rejects(
    new Agent().ask({ turn: "t4", prompt: "deny" }),
    (error) =>
      error instanceof SourceDenied &&
      error.sessionId === "default" &&
      error.turnId === "t4",
  );
  const seen = service.events();
  for (const [sessionId, turnId] of [
    ["extracted", "t1"],
    ["als", "t2"],
    ["resolved", "t3"],
  ]) {
    const group = seen.filter(({ session_id }) => session_id === sessionId);
    assert.deepEqual(
      group.map(({ event }) => event),
      ["session.start", "input.submit", "turn.stop", "session.end"],
    );
    assert.deepEqual(
      group.filter(({ turn_id }) => turn_id).map(({ turn_id }) => turn_id),
      [turnId, turnId],
    );
  }
  await main.teardown();
});

test("drain uses the activation epoch, waits for admitted calls, and gates cached references", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": "exports.run = async ({ value, gate }) => (await gate, value);",
  });
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(
    t,
    profile([target("run", { call: "async" })]),
    { resolveFrom: entry },
  );
  const sdk = createRequire(entry)("profile-fixture");
  const cached = sdk.run;
  const old = Promise.withResolvers();
  const early = cached({ value: "early", gate: old.promise });
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  old.resolve();
  assert.equal(await early, "early");
  assert.deepEqual(service.events(), []);
  const late = Promise.withResolvers();
  const admitted = cached({ value: "admitted", gate: late.promise });
  await assert.rejects(source.teardown(20), {
    code: "source_cleanup_unconfirmed",
  });
  assert.equal(source.state, "draining");
  assert.notEqual(sdk.run, cached);
  assert.throws(() => cached({}), { code: "source_unavailable" });
  late.resolve();
  assert.equal(await admitted, "admitted");
  await source.teardown();
  assert.equal(source.state, "closed");
  assert.equal(service.events().length, 1);
  assert.throws(() => cached({}), { code: "source_unavailable" });
});

test("stream return, throw and AbortSignal cancel native iteration and discharge the ledger", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.closed = [];
      exports.sync = function* () { try { yield "sync"; } finally { exports.closed.push("sync"); } };
      exports.async = async function* (options) {
        try { yield "async"; } finally {
          exports.closed.push("async");
          options?.signal.throwIfAborted();
        }
      };
    `,
  });
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(
    t,
    profile([
      target("sync", { call: "sync_iter" }),
      target("async", { call: "async_iter" }),
    ]),
    { resolveFrom: entry },
  );
  const sdk = createRequire(entry)("profile-fixture");
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const first = sdk.sync();
  assert.deepEqual(first.next(), { value: "sync", done: false });
  assert.deepEqual(first.return("closed"), { value: "closed", done: true });
  const second = sdk.async();
  assert.deepEqual(await second.next(), { value: "async", done: false });
  await assert.rejects(second.throw(new Error("native throw")), /native throw/);
  const controller = new AbortController();
  const third = sdk.async({ signal: controller.signal });
  await third.next();
  controller.abort(new Error("aborted stream"));
  await assert.rejects(third.next(), /aborted stream/);
  const unopened = sdk.async();
  await unopened.return();
  const idle = sdk.async();
  const active = sdk.async();
  const firstRead = active.next();
  await assert.rejects(source.teardown(20), {
    code: "source_cleanup_unconfirmed",
  });
  assert.deepEqual(await firstRead, { done: false, value: "async" });
  await active.return();
  const events = service.events();
  await source.teardown(500);
  assert.equal(source.state, "closed");
  await assert.rejects(idle.next(), { code: "source_unavailable" });
  assert.deepEqual(service.events(), events);
  assert.deepEqual(sdk.closed, ["sync", "async", "async", "async"]);
});

test("native fallback allows cached calls through draining and reports no SDK stdout", async (t) => {
  const { entry } = appFixture(t, {
    "main.cjs": "exports.f = (value) => value;",
  });
  const source = await installProfile(t, profile([target("f")]), {
    resolveFrom: entry,
    availability: "native_fallback",
  });
  const { f } = createRequire(entry)("profile-fixture");
  await source.teardown();
  assert.equal(f("native"), "native");
});

test(
  "real AW admits a synchronous JavaScript seam and releases its SDK Scope",
  {
    skip: !BINARY,
    timeout: 15000,
  },
  async (t) => {
    const { root, entry } = appFixture(t, {
      "main.cjs": `exports.ask = ({ prompt }) => prompt;
      exports.run = ({ payload }) => payload.command;`,
    });
    const service = await startAwService(root);
    t.after(() => service.stop());
    const source = await installProfile(
      t,
      profile([
        target("ask", {
          hooks: inputHook,
          fields: { prompt: field("arg.0.prompt") },
        }),
        target("run", { hooks: toolHooks, fields: tools() }),
      ]),
      { resolveFrom: entry },
    );
    const sdk = createRequire(entry)("profile-fixture");
    await source.activate({ endpoint: service.endpoint, target: "app" });
    assert.equal(sdk.ask({ prompt: "hello" }), "hello");
    assert.throws(() => sdk.ask({ prompt: "deny" }), SourceDenied);
    assert.equal(
      sdk.run({ payload: { command: "raw:SECRET" } }),
      "safe:[REDACTED]",
    );
    assert.throws(
      () => sdk.run({ payload: { command: "FAULT" } }),
      SourceResultWithheld,
    );
    await source.teardown();
    assert.deepEqual(
      readProviderLog(service.logPath).map(({ op }) => op),
      [
        "guard",
        "guard",
        "rewrite",
        "guard",
        "filter",
        "rewrite",
        "guard",
        "filter",
      ],
    );
  },
);

test(
  "descriptor activation and cooperative aw connect --sdk-pid reuse the public SDK control path",
  {
    skip: !BINARY,
    timeout: 15000,
  },
  async (t) => {
    const execute = promisify(execFile);
    const { root, entry } = appFixture(t, {
      "main.cjs": `exports.ask = async ({ prompt }) => prompt;
      exports.run = async ({ payload }) => payload.command;`,
    });
    const service = await startAwService(root);
    t.after(() => service.stop());
    const source = await installProfile(
      t,
      profile([
        target("ask", {
          call: "async",
          hooks: inputHook,
          fields: { prompt: field("arg.0.prompt") },
        }),
        target("run", { call: "async", hooks: toolHooks, fields: tools() }),
      ]),
      { resolveFrom: entry },
    );
    const configFile = join(root, "app-config.json");
    const config = "{}";
    writeFileSync(configFile, config, { mode: 0o600 });
    const descriptorFile = join(root, "descriptor.json");
    writeFileSync(
      descriptorFile,
      JSON.stringify({
        version: "aw-deployment/v1alpha1",
        configuration_sha256: createHash("sha256").update(config).digest("hex"),
        target: "app",
        endpoint: service.endpoint,
        availability: "required",
        profile: {
          adapter: "sdk",
          version: "aw-hook-source/v1",
          entrypoint: "application",
        },
        wait_for_input_submit: true,
      }),
      { mode: 0o600 },
    );
    const activating = source.activate({
      descriptorFile,
      configFile,
      cooperative: true,
      reconnectMs: 100,
    });
    activating.catch(() => {});
    const cli = (...args) => execute(BINARY, args, { timeout: 4000 });
    try {
      for (let tries = 0; ; tries++) {
        const { stdout } = await cli(
          "instance",
          "list",
          "--socket",
          service.endpoint,
        );
        if (JSON.parse(stdout).applications.length) break;
        assert(tries < 50, "expected SDK control application");
        await delay(20);
      }
      await cli(
        "connect",
        "--socket",
        service.endpoint,
        "--sdk-pid",
        String(process.pid),
      );
      await activating;
      const sdk = createRequire(entry)("profile-fixture");
      assert.equal(await sdk.ask({ prompt: "connected" }), "connected");
      assert.equal(
        await sdk.run({ payload: { command: "raw:connected" } }),
        "safe:connected",
      );
    } finally {
      await source.teardown();
    }
  },
);

test("required activation failure keeps wrappers closed and cleanup remains confirmed", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": "exports.f = () => 42;",
  });
  const source = await installProfile(t, profile([target("f")]), {
    resolveFrom: entry,
  });
  const { f } = createRequire(entry)("profile-fixture");
  await assert.rejects(
    source.activate({
      endpoint: join(root, "missing.sock"),
      target: "fixture",
      timeoutMs: 100,
    }),
    { code: "ENOENT" },
  );
  assert.equal(source.state, "poisoned");
  assert.throws(f, { code: "source_unavailable" });
  await source.teardown();
  assert.equal(source.state, "closed");
});

test("native fallback preserves admission refusal and accepts an unavailable service", async (t) => {
  const { root, entry } = appFixture(t, { "main.cjs": "exports.f = () => 1;" });
  const document = profile([target("f")]);
  const options = { resolveFrom: entry, availability: "native_fallback" };
  const service = await policyFixture(t, root, [
    { event: "tool.after", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(t, document, options);
  const require = createRequire(entry);
  const { f } = require("profile-fixture");
  await assert.rejects(
    source.activate({ endpoint: service.endpoint, target: "fixture" }),
    { code: "ready_not_confirmed" },
  );
  assert.throws(f, { code: "source_unavailable" });
  await source.teardown();
  assert.throws(f, { code: "source_unavailable" });
  const offline = await installProfile(t, document, options);
  await offline.activate({
    endpoint: join(root, "missing.sock"),
    target: "fixture",
  });
  assert.equal(require("profile-fixture").f(), 1);
  await offline.teardown();
});

for (const action of ["return", "abort"])
  test(`teardown reports a native iterator cleanup failure after ${action}`, async (t) => {
    const { root, entry } = appFixture(t, {
      "main.cjs": `exports.read = async function* () {
    try { yield "first"; } finally { throw new Error("native cleanup failed"); }
  };`,
    });
    const service = await policyFixture(t, root, [
      { event: "session.start", step: "record", budget_ms: 1000 },
    ]);
    const source = await ProfileSource.probe(
      profile([target("read", { call: "async_iter" })]),
      { resolveFrom: entry },
    );
    source.install();
    await source.verify();
    await source.activate({ endpoint: service.endpoint, target: "fixture" });
    const controller = new AbortController();
    const stream = createRequire(entry)("profile-fixture").read({
      signal: controller.signal,
    });
    assert.equal((await stream.next()).value, "first");
    if (action === "return")
      await assert.rejects(stream.return(), {
        code: "source_cleanup_unconfirmed",
        toolExecuted: true,
      });
    else controller.abort();
    await assert.rejects(
      source.teardown(),
      (error) =>
        error.code === "source_cleanup_unconfirmed" &&
        error.cause.message === "native cleanup failed",
    );
  });
