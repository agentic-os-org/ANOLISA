/** Resident admission retains native ownership across setup failures and daemon shutdown. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { Resident, bounded } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";
import { managedDaemon } from "../../../../aw-adapter/native/qoder-sdk/daemon.mjs";
import { managedMcpServer } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";
import { PRIVATE_EXECUTION_VERSION } from "../../../../aw-adapter/native/qoder-sdk/private.mjs";

import { fixture, deferred } from "./resident-support.mjs";

const missingSeams = [
  ["recordInboundSessionMessage", query => { delete query.recordInboundSessionMessage; },
    /recordInboundSessionMessage/],
  ["transport.write", query => { delete query.transport.write; }, /native control response transport/],
  ["processControlRequest", query => { delete query.processControlRequest; }, /native MCP dispatch seam/],
];

for (const [name, remove, message] of missingSeams) {
  test(`missing ${name} joins native close before releasing the scope`, { timeout: 5000 }, async t => {
    const f = fixture(t);
    remove(f.state.query);
    const options = {};
    if (name === "processControlRequest") {
      const server = managedMcpServer({ createSdkMcpServer: () => ({ type: "sdk" }) }, { tools: [] });
      options.mcpServers = { fixture: server };
    }
    f.state.importGate.resolve();
    const pending = f.create(options);
    await Promise.race([f.state.closeEntered.promise, pending]);
    assert.equal(f.state.created, 1);
    assert.equal(f.state.nativeCloses, 1);
    assert.equal(f.state.scopeCloses, 0, "scope must remain owned during native close");
    assert(f.scope.handle);
    assert.equal(f.state.settled, false);
    assert.equal(f.state.ready, 0);
    f.state.closeGate.resolve();
    const result = await pending;
    assert.match(result.error?.message ?? "", message);
    assert.deepEqual(f.state.events, ["native-closed", "scope-closed"]);
    await f.resident.close();
    assert.equal(f.state.nativeCloses, 1, "cleanup must call the original close exactly once");
    assert.equal(f.state.scopeCloses, 1);
  });
}

test("missing native close retains the scope after admission fails", { timeout: 5000 }, async t => {
  const f = fixture(t);
  delete f.state.query.close;
  f.state.importGate.resolve();
  const result = await f.create();
  assert(result.error);
  assert.equal(f.state.created, 1);
  assert.equal(f.state.nativeCloses, 0);
  assert.equal(f.state.ready, 0);
  assert.equal(f.state.scopeCloses, 0);
  assert(f.scope.handle, "unconfirmed native cleanup must retain ownership");
  assert.equal(f.scope.failed, true);
  await assert.rejects(f.resident.close());
  assert.equal(f.state.scopeCloses, 0);
});

test("daemon stop during SDK import prevents late Query creation", { timeout: 5000 }, async t => {
  const f = fixture(t);
  let stops = 0;
  const daemon = await managedDaemon({
    async startDaemon() { return { async stop() { stops++; } }; },
  }, {}, f.config, bounded);
  f.state.daemon = daemon;
  const pending = f.create({ daemon });
  await f.state.imported.promise;
  assert.equal(f.state.created, 0);
  await daemon.stop();
  assert.equal(stops, 1);
  f.state.importGate.resolve();
  const result = await pending;
  assert.equal(f.state.created, 0, "a stopped daemon cannot acquire a new Query");
  assert.match(result.error?.message ?? "", /daemon stopped/);
  assert.equal(f.state.ready, 0);
  assert.equal(f.state.scopeCloses, 1);
  assert.equal(f.scope.handle, null);
});

async function daemon(f) {
  f.state.daemon = await managedDaemon({
    async startDaemon() {
      return { async stop() { f.state.events.push("daemon-stopped"); } };
    },
  }, {}, f.config, bounded);
  return f.state.daemon;
}

test("resident Query inherits its daemon site and rejects a different site before SDK creation", { timeout: 5000 }, async t => {
  for (const mismatch of [true, false]) {
    const f = fixture(t);
    const cn = path.join(path.dirname(f.config.sdk), "cn.mjs");
    fs.copyFileSync(f.config.sdk, cn);
    f.config.sdks = { global: f.config.sdk, cn };
    f.config.default_site = "global";
    const owner = await managedDaemon({
      async startDaemon() { return { async stop() {} }; },
    }, {}, { ...f.config, sdk: cn, sdk_site: "cn" }, bounded);
    f.state.daemon = owner;
    f.state.importGate.resolve();
    const result = await f.create({ daemon: owner, ...(mismatch ? { awSite: "global" } : {}) });
    if (mismatch) {
      assert.match(result.error?.message ?? "", /site differs/);
      assert.equal(f.state.created, 0);
      assert.equal(f.state.executions, 0);
      assert.equal(f.state.ready, 0);
      assert.equal(f.scope.handle, null);
    } else {
      assert.ifError(result.error);
      await f.state.dispatch;
      assert.equal(f.state.created, 1);
      assert.equal(f.state.executions, 1);
      assert.equal(f.state.ready, 1);
      assert.equal(f.resident.config.sdk, cn);
      assert.equal(f.resident.config.active_site, "cn");
      assert.equal(f.state.options.awSite, undefined);
      f.state.closeGate.resolve();
      await result.value.close();
    }
  }
});

test("daemon stop during initialization prevents readiness and execution", { timeout: 5000 }, async t => {
  const f = fixture(t, true);
  const owner = await daemon(f);
  f.state.importGate.resolve();
  const pending = f.create({ daemon: owner });
  await f.state.initializationEntered.promise;
  const stopping = owner.stop();
  await f.state.closeEntered.promise;
  f.state.initializationGate.resolve();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.state.ready, 0);
  assert.equal(f.state.settled, false, "failed creation must await native cleanup");
  assert.equal(f.state.scopeCloses, 0);
  f.state.closeGate.resolve();
  const result = await pending;
  await stopping;
  await f.state.dispatch;
  assert.match(result.error?.message ?? "", /closed|stopped/);
  assert.equal(result.value, undefined);
  assert.equal(f.state.executions, 0);
  assert.equal(f.state.nativeCloses, 1);
  assert.deepEqual(f.state.events, ["native-closed", "scope-closed", "daemon-stopped"]);
});

test("read-loop failure during initialization prevents readiness and execution", { timeout: 5000 }, async t => {
  const f = fixture(t, true);
  const readLoop = deferred();
  f.state.query.readLoopPromise = readLoop.promise;
  f.state.importGate.resolve();
  const pending = f.create();
  await f.state.initializationEntered.promise;
  const failure = new Error("native transport failed during initialization");
  readLoop.reject(failure);
  await f.state.closeEntered.promise;
  f.state.initializationGate.resolve();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(f.state.ready, 0);
  assert.equal(f.state.settled, false);
  assert.equal(f.state.scopeCloses, 0);
  f.state.closeGate.resolve();
  const result = await pending;
  await f.state.dispatch;
  assert.equal(result.error, failure);
  assert.equal(result.value, undefined);
  assert.equal(f.state.executions, 0);
  assert.equal(f.state.nativeCloses, 1);
  assert.deepEqual(f.state.events, ["native-closed", "scope-closed"]);
});

for (const earlierFailure of [false, true]) {
  for (const cleanupFails of [false, true]) {
    test(`initialization retains ${earlierFailure ? "earlier read-loop" : "initialization"} failure with ${cleanupFails ? "failed" : "successful"} cleanup`,
      { timeout: 5000 }, async t => {
        const f = fixture(t, true);
        const readLoop = deferred();
        if (earlierFailure) f.state.query.readLoopPromise = readLoop.promise;
        f.state.importGate.resolve();
        const pending = f.create();
        await f.state.initializationEntered.promise;
        const first = new Error("first read-loop failure");
        const initialization = new Error("initialization waiter rejected");
        const cleanup = new Error("native close rejected");
        const primary = earlierFailure ? first : initialization;
        if (earlierFailure) {
          readLoop.reject(first);
          await f.state.closeEntered.promise;
        }
        f.state.initializationGate.reject(initialization);
        await f.state.closeEntered.promise;
        await new Promise(resolve => setImmediate(resolve));
        assert.equal(f.resident.failure, primary);
        assert.equal(f.state.settled, false, "creation must join native cleanup before rejecting");
        assert.equal(f.state.ready, 0);
        assert.equal(f.state.scopeCloses, 0);
        if (cleanupFails) f.state.closeGate.reject(cleanup);
        else f.state.closeGate.resolve();
        const result = await pending;
        await f.state.dispatch;
        assert.equal(f.resident.failure, primary);
        assert.equal(result.value, undefined);
        assert.equal(f.state.executions, 0);
        assert.equal(f.state.nativeCloses, 1);
        assert.equal(f.scope.failed, true);
        if (cleanupFails) {
          assert(result.error instanceof AggregateError);
          assert.equal(result.error.errors[0], primary);
          const cleanupFailure = await f.resident.close().catch(error => error);
          assert.equal(result.error.errors[1], cleanupFailure);
          assert.equal(cleanupFailure.errors[0].errors[0], cleanup);
          assert.equal(f.state.scopeCloses, 0);
          assert(f.scope.handle, "unconfirmed native cleanup must retain ownership");
        } else {
          assert.equal(result.error, primary);
          assert.equal(f.state.scopeCloses, 1);
          assert.equal(f.scope.handle, null);
        }
      });
  }
}

test("malformed native control frames retain their original rejection and fence the Query", { timeout: 5000 }, async t => {
  for (const frame of [undefined, null, { request_id: "missing-request" },
    { request_id: "null-request", request: null }]) {
    const f = fixture(t);
    const responses = [];
    f.state.query.transport.write = line => responses.push(JSON.parse(line));
    f.state.importGate.resolve();
    const result = await f.create();
    assert.ifError(result.error);
    let primary;
    const ticket = f.resident.identities.ticket.bind(f.resident.identities);
    f.resident.identities.ticket = value => {
      try { return ticket(value); }
      catch (error) { primary = error; throw error; }
    };
    await f.state.query.handleControlRequest(frame);
    assert(primary instanceof Error);
    assert.equal(f.resident.failure, primary);
    assert.equal(f.scope.failed, true);
    assert.deepEqual(responses, [{ type: "control_response", response: {
      subtype: "error", ...(frame?.request_id ? { request_id: frame.request_id } : {}),
      error: "AW native identity rejected" } }]);
    f.state.closeGate.resolve();
    await assert.rejects(result.value.next(), error => error === primary);
    assert.equal(f.state.nativeCloses, 1);
    assert.equal(f.state.scopeCloses, 1);
  }
});

test("failed native rejection writes preserve the identity failure and fence the Query", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const writeFailure = new Error("native rejection write failed");
  f.state.query.transport.write = () => { throw writeFailure; };
  f.state.importGate.resolve();
  const result = await f.create();
  assert.ifError(result.error);
  assert.throws(() => f.state.query.handleControlRequest({ request_id: "unknown", request: {
    subtype: "can_use_tool", tool_use_id: "unregistered" } }), error => error === writeFailure);
  const primary = f.resident.failure;
  assert.match(primary?.message ?? "", /unknown native tool identity/);
  assert.equal(f.scope.failed, true);
  f.state.closeGate.resolve();
  await assert.rejects(result.value.next(), error => error === primary);
  assert.equal(f.state.nativeCloses, 1);
  assert.equal(f.state.scopeCloses, 1);
});

test("daemon stop waits for an initialized Query to release its scope", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const owner = await daemon(f);
  f.state.importGate.resolve();
  const result = await f.create({ daemon: owner });
  assert.ifError(result.error);
  assert.equal(f.state.ready, 1);
  await f.state.dispatch;
  assert.equal(f.state.executions, 1);
  let stopped = false;
  const stopping = owner.stop().then(() => { stopped = true; });
  await f.state.closeEntered.promise;
  assert.equal(stopped, false);
  assert.equal(f.state.scopeCloses, 0);
  assert(f.scope.handle);
  assert.throws(() => result.value.streamInput([]), /closed|unbound/);
  assert(!f.state.events.includes("daemon-stopped"));
  f.state.closeGate.resolve();
  await stopping;
  assert.equal(f.state.nativeCloses, 1);
  assert.equal(f.state.scopeCloses, 1);
  assert.equal(f.scope.handle, null);
  assert.deepEqual(f.state.events, ["native-closed", "scope-closed", "daemon-stopped"]);
});

for (const mode of ["tool-failure", "cancel"]) {
  test(`private ${mode} drains credentials and retires the Query scope`, { timeout: 3000 }, async t => {
    const f = fixture(t);
    const entered = deferred(), finalizing = deferred(), finish = deferred();
    let released = false;
    f.config.resident_policy_reload = true;
    f.scope.reserve = async () => { f.scope.reservation = {}; };
    f.scope.policy = async () => ({ policy_revision: "a".repeat(64), hooks: [], events: {} });
    f.scope.acknowledge = async () => {};
    f.scope.release = async () => {
      assert(released, "root reservation must outlive credential release");
      assert(f.scope.closed, "root reservation must outlive child cleanup");
      f.state.events.push("root-released");
      f.scope.reservation = null;
    };
    f.state.importGate.resolve();
    const result = await f.create({ awExecutionFactory: async () => ({
      version: PRIVATE_EXECUTION_VERSION, environment: {}, hookEnvironment: {}, secrets: {},
      redact: value => value,
      release: async () => { released = true; f.state.events.push("lease-released"); },
    }) });
    assert.ifError(result.error);
    assert.equal(Object.hasOwn(f.state.options, "awExecutionFactory"), false);
    const operation = assert.rejects(f.resident.executionOwner.run({ invocationId: "private",
      toolName: "fixture", input: {} }, async (_context, { signal }) => {
      try {
        entered.resolve();
        if (mode === "tool-failure") throw new Error("private business failure");
        await new Promise(resolve => signal.addEventListener("abort", resolve, { once: true }));
      } finally {
        finalizing.resolve();
        await finish.promise;
        assert.equal(released, false);
        f.state.events.push("handler-finally");
      }
    }), /withheld/);
    await entered.promise;
    const closing = result.value.close();
    await f.state.closeEntered.promise;
    f.state.closeGate.resolve();
    await finalizing.promise;
    assert.equal(f.state.scopeCloses, 0);
    finish.resolve();
    await Promise.all([operation, closing]);
    assert.equal(f.state.scopeCloses, 1);
    assert.equal(f.scope.handle, null);
    assert.deepEqual(f.state.events, ["native-closed", "handler-finally", "lease-released", "scope-closed", "root-released"]);
  });
}

async function retirementFixture(t, options = {}) {
  const f = fixture(t);
  f.config.resident_policy_reload = true;
  f.scope.reserve = async () => { f.scope.reservation = {}; };
  f.scope.policy = async () => ({ policy_revision: "a".repeat(64), hooks: [], events: {} });
  f.scope.acknowledge = async () => {};
  f.scope.drain = async () => { f.state.events.push("scope-drained"); };
  f.scope.release = async () => {
    f.state.events.push("root-released");
    f.scope.reservation = null;
  };
  const owner = await daemon(f);
  f.state.importGate.resolve();
  const result = await f.create({ daemon: owner, ...options });
  assert.ifError(result.error);
  assert(f.resident.owner.queries.has(f.resident.ownerClose));
  return { ...f, query: result.value, owner };
}

function assertRetained(f) {
  assert(f.scope.handle, "failed cleanup must retain the child scope");
  assert(f.scope.reservation, "failed cleanup must retain the root reservation");
  assert(f.resident.owner.queries.has(f.resident.ownerClose), "daemon must retain the Query owner");
  assert.equal(f.state.scopeCloses, 0);
  assert(!f.state.events.includes("scope-drained"));
}

for (const failure of ["missing", "rejected"]) {
  test(`${failure} native close retains snapshots, scope and daemon ownership`, { timeout: 5000 }, async t => {
    const f = await retirementFixture(t);
    f.resident.nativeAssets.close = async () => { f.state.events.push("assets-closed"); };
    f.resident.nativeClose = failure === "missing" ? undefined : async () => {
      throw new Error("native close failed");
    };
    f.state.daemon = { stop: () => f.owner.stop().catch(() => {}) };
    await assert.rejects(f.query.close(), /scope retained/);
    assertRetained(f);
    assert(!f.state.events.includes("assets-closed"));
    assert.equal(f.scope.failed, true);
    await assert.rejects(f.query.close(), /scope retained/);
    assertRetained(f);
  });
}

test("callback finally exceeding its cleanup deadline retains snapshots and ownership", { timeout: 5000 }, async t => {
  const f = await retirementFixture(t);
  const finalizing = deferred(), finish = deferred();
  const callback = f.resident.track(async () => {
    try { await f.state.closeGate.promise; }
    finally {
      finalizing.resolve();
      await finish.promise;
      f.state.events.push("callback-finally");
    }
  });
  f.resident.nativeAssets.close = async () => { f.state.events.push("assets-closed"); };
  f.resident.cleanupBudget = () => 10;
  f.state.daemon = { stop: () => f.owner.stop().catch(() => {}) };
  f.state.closeGate.resolve();
  try {
    const closing = f.query.close();
    await finalizing.promise;
    await assert.rejects(closing, error => error instanceof AggregateError &&
      error.errors.some(cause => /callback cleanup deadline/.test(cause.message)));
    assert.equal(f.resident.callbacks.size, 1, "original finally is still running");
    assert(!f.state.events.includes("assets-closed"));
    assertRetained(f);
  } finally {
    finish.resolve();
    await callback;
  }
  await assert.rejects(f.query.close(), /scope retained/);
  assertRetained(f);
  assert(!f.state.events.includes("assets-closed"), "a late finally cannot reopen failed cleanup");
});

test("snapshot removal IO failure rejects without draining or releasing ownership", { timeout: 5000 }, async t => {
  const f = fixture(t);
  const root = path.dirname(f.config.binding);
  const plugin = path.join(root, "plugin");
  fs.mkdirSync(plugin);
  fs.writeFileSync(path.join(plugin, "plugin.json"), JSON.stringify({ name: "cleanup-fixture" }));
  f.config.native_policy = { declarative_plugins: true };
  f.state.query.request = async () => ({ sources: { flag: { skills: { disableShellExecution: true } } },
    effective: { skills: { disableShellExecution: true } } });
  const owner = await daemon(f);
  f.state.importGate.resolve();
  const result = await f.create({ daemon: owner, plugins: [{ type: "local", path: plugin }] });
  assert.ifError(result.error);
  f.scope.drain = async () => { f.state.events.push("scope-drained"); };
  const moved = `${root}-moved`;
  fs.renameSync(root, moved);
  t.after(() => fs.rmSync(moved, { recursive: true, force: true }));
  fs.writeFileSync(root, "ancestor replaced with a file");
  f.state.daemon = { stop: () => owner.stop().catch(() => {}) };
  f.state.closeGate.resolve();
  await assert.rejects(result.value.close(), { code: "ENOTDIR" });
  assert(f.scope.handle);
  assert.equal(f.state.scopeCloses, 0);
  assert(!f.state.events.includes("scope-drained"));
  assert(f.resident.owner.queries.has(f.resident.ownerClose));
  await assert.rejects(result.value.close(), { code: "ENOTDIR" });
});

test("successful cleanup joins callback finally before snapshots and scope retirement", { timeout: 5000 }, async t => {
  const f = await retirementFixture(t);
  const finalizing = deferred(), finish = deferred();
  const callback = f.resident.track(async () => {
    try { await f.state.closeGate.promise; }
    finally {
      finalizing.resolve();
      await finish.promise;
      f.state.events.push("callback-finally");
    }
  });
  f.resident.nativeAssets.close = async () => { f.state.events.push("assets-closed"); };
  const closing = f.query.close();
  f.state.closeGate.resolve();
  try {
    await f.resident.nativeStopped;
    await finalizing.promise;
    assertRetained(f);
    assert(!f.state.events.includes("assets-closed"));
  } finally { finish.resolve(); }
  await Promise.all([callback, closing]);
  assert.deepEqual(f.state.events, ["native-closed", "callback-finally", "assets-closed",
    "scope-drained", "scope-closed", "root-released"]);
  assert.equal(f.scope.handle, null);
  assert.equal(f.scope.reservation, null);
  assert(!f.resident.owner.queries.has(f.resident.ownerClose));
});


test("native close can drain past the old twelve-second deadline", { timeout: 5000 }, async t => {
  const f = fixture(t);
  f.state.importGate.resolve();
  assert(!(await f.create()).error);
  t.mock.timers.enable({ apis: ["setTimeout", "Date"] });
  let settled = false;
  const closing = f.resident.close().finally(() => { settled = true; });
  closing.catch(() => {});
  try {
    await f.state.closeEntered.promise;
    t.mock.timers.tick(13000);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(settled, false, "native close must retain its shared sixty-second budget");
    assert(f.scope.handle, "scope must remain owned until native close completes");
    f.state.closeGate.resolve();
    await closing;
    assert.deepEqual(f.state.events, ["native-closed", "scope-closed"]);
  } finally {
    f.state.closeGate.resolve();
    await closing.catch(() => {});
    t.mock.timers.reset();
  }
});
