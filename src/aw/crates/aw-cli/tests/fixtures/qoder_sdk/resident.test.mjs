/** Resident admission retains native ownership across setup failures and daemon shutdown. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { Resident, bounded } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";
import { managedDaemon } from "../../../../aw-adapter/native/qoder-sdk/daemon.mjs";
import { managedMcpServer } from "../../../../aw-adapter/native/qoder-sdk/mcp.mjs";

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};

function fixture(t, holdInitialization = false) {
  const root = fs.mkdtempSync(path.join(process.cwd(), "sdk-resident-"));
  const sdk = path.join(root, "sdk.mjs");
  const state = { imported: deferred(), importGate: deferred(), closeEntered: deferred(),
    closeGate: deferred(), created: 0, ready: 0, nativeCloses: 0, scopeCloses: 0,
    initializationEntered: deferred(), initializationGate: deferred(), executions: 0,
    settled: false, events: [] };
  if (!holdInitialization) state.initializationGate.resolve();
  globalThis[root] = state;
  fs.writeFileSync(sdk, `const state = globalThis[${JSON.stringify(root)}];
state.imported.resolve();
await state.importGate.promise;
export function query({ prompt }) {
  state.created++;
  state.dispatch = prompt.next().then(item => { if (!item.done) state.executions++; });
  return state.query;
}
`);
  state.query = {
    recordInboundSessionMessage() {},
    async handleControlRequest() {},
    async processControlRequest() {},
    transport: { write() {} },
    async interrupt() {},
    async getContextUsage() {},
    async next() { return { done: true }; },
    async streamInput() {},
    async initializationResult() {
      state.initializationEntered.resolve();
      await state.initializationGate.promise;
      return { capabilities: ["msg_lifecycle_v1"] };
    },
    async close() {
      state.nativeCloses++;
      state.closeEntered.resolve();
      await state.closeGate.promise;
      state.events.push("native-closed");
    },
  };
  const config = { sdk, binding: path.join(root, "binding"), hooks: [] };
  const resident = new Resident(config);
  const scope = resident.scope;
  scope.open = async () => { scope.handle = {}; };
  scope.close = async () => {
    state.scopeCloses++;
    state.events.push("scope-closed");
    scope.handle = null;
    scope.closed = true;
  };
  const create = (extra = {}) => {
    state.pending = resident.create("fixture", { cwd: process.cwd(), ...extra },
      (_config, options) => ({ ...options, hooks: {} }), () => { state.ready++; })
      .then(value => ({ value }), error => ({ error }))
      .finally(() => { state.settled = true; });
    return state.pending;
  };
  t.after(async () => {
    state.importGate.resolve();
    state.initializationGate.resolve();
    state.closeGate.resolve();
    try {
      await state.pending;
      await state.dispatch;
      await resident.close().catch(() => {});
      await state.daemon?.stop();
    } finally {
      delete globalThis[root];
      fs.rmSync(root, { recursive: true, force: true });
    }
  });
  return { state, config, resident, scope, create };
}

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
