/** Exercise execution fences and retirement acknowledgement without an SDK install. */
import assert from "node:assert/strict";
import { after } from "node:test";
import test from "node:test";
import { mkdtempSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Scope } from "../../crates/aw-adapter/native/pi/scope.mjs";

const root = mkdtempSync(join(tmpdir(), "aw-pi-scope-"));
const previous = process.env.AW_NATIVE_CONFIG;
process.env.AW_NATIVE_CONFIG = join(root, "config.json");
writeFileSync(process.env.AW_NATIVE_CONFIG, "{}");
after(() => {
  if (previous === undefined) delete process.env.AW_NATIVE_CONFIG;
  else process.env.AW_NATIVE_CONFIG = previous;
  rmSync(root, { recursive: true });
  assert(!existsSync(root));
});
const create = () => {
  const scope = new Scope();
  scope.handle = {};
  scope.invocationLimit = 4;
  scope.command = async () => ({ code: 0 });
  return scope;
};
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

test("retirement transport failure latches even outside prompt cleanup", async () => {
  const scope = create();
  scope.invocations.set("call", 1);
  scope.command = async () => { throw new Error("callback_timeout"); };
  await assert.rejects(scope.finish("call"), /callback_timeout/);
  assert(scope.failed);
  assert.equal(scope.invocations.get("call"), 1);
  assert.throws(() => scope.check());
});

test("retirement nonzero reply retains identity and fences reuse", async () => {
  const scope = create();
  scope.invocations.set("call", 1);
  scope.command = async () => ({ code: 1 });
  await assert.rejects(scope.finish("call"), /drain failed/);
  assert(scope.failed);
  assert.equal(scope.invocations.get("call"), 1);
});

test("drain failure cannot admit another execution or registration", async () => {
  const scope = create();
  scope.begin();
  await scope.register("call", "tool", {});
  scope.command = async () => { throw new Error("callback_timeout"); };
  await assert.rejects(scope.drain(), /callback_timeout/);
  assert(scope.failed);
  assert.equal(scope.running, false);
  await assert.rejects(scope.register("late", "tool", {}));
  assert.throws(() => scope.begin());
  assert.deepEqual([...scope.invocations.keys()], ["call"]);
});

test("drain joins registration before retirement and fences queued registration", async () => {
  const scope = create();
  scope.begin();
  const entered = deferred(), release = deferred();
  const order = [];
  scope.command = async (action) => {
    if (action === "register") { entered.resolve(); await release.promise; }
    order.push(action);
    return { code: 0 };
  };
  const registering = scope.register("call", "tool", {});
  await entered.promise;
  const queued = assert.rejects(scope.register("queued", "tool", {}));
  const draining = scope.drain();
  try {
    await Promise.resolve();
    assert.deepEqual(order, []);
    assert.throws(() => scope.begin());
  } finally { release.resolve(); }
  await Promise.all([registering, queued, draining]);
  assert.deepEqual(order, ["register", "finish"]);
  assert.equal(scope.invocations.size, 0);
});

test("successful drain rejects late calls and permits a new execution", async () => {
  const scope = create();
  assert.throws(() => scope.checkExecution());
  scope.begin();
  const execution = scope.execution;
  await scope.register("first", "tool", {});
  await scope.drain();
  assert.equal(scope.failed, false);
  await assert.rejects(scope.register("late", "tool", {}));
  assert.equal(scope.invocations.size, 0);
  scope.begin();
  assert.throws(() => scope.checkExecution(execution));
  await scope.register("fresh", "tool", {});
  await scope.drain();
  assert.equal(scope.invocations.size, 0);
});

test("bridge checks latch transport loss and keep independent scopes usable", async () => {
  const scope = create(), other = create();
  scope.policyRevision = "a".repeat(64);
  scope.monitorAbort = new AbortController();
  let calls = 0;
  scope.command = async () => {
    if (++calls === 1) return { code: 0 };
    throw new Error("service restarted");
  };
  await scope.validate();
  await assert.rejects(scope.validate(), /service restarted/);
  assert.throws(() => scope.begin());
  other.begin();
  await other.register("independent", "tool", {});
  await other.drain();
  scope.command = async () => ({ code: 0 });
  await assert.rejects(scope.validate());
  await scope.close();
});

test("idle bridge probe reports failure and stops its timer", { timeout: 2000 }, async t => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const scope = create(), reported = deferred();
  let installed = true, reports = 0;
  scope.probe = () => { if (!installed) throw new Error("removed native seam"); };
  scope.command = async (action, value) => {
    if (action === "watch") return { code: 0, stdout: JSON.stringify({
      policy_revision: "b".repeat(64), interval_ms: 2000,
    }) };
    if (action === "check") { reports++; reported.resolve(value); return { code: 1 }; }
    return { code: 0 };
  };
  await scope.watch();
  installed = false;
  t.mock.timers.tick(2000);
  const report = await reported.promise;
  assert.equal(report.valid, false);
  assert.equal(report.policy_revision, "b".repeat(64));
  await scope.close();
  const count = reports;
  t.mock.timers.tick(10000);
  assert.equal(reports, count);
  assert(scope.failed);
});

test("concurrent close joins one original cleanup and retains its failure", async () => {
  const scope = create(), entered = deferred(), gate = deferred();
  let closed = 0;
  scope.command = async () => { closed++; entered.resolve(); await gate.promise; return { code: 1 }; };
  const first = scope.close();
  await entered.promise;
  const second = scope.close();
  assert.equal(first, second);
  gate.resolve();
  await assert.rejects(first, /release failed/);
  await assert.rejects(second, /release failed/);
  await assert.rejects(scope.close(), /release failed/);
  assert.equal(closed, 1);
});

test("close joins late admission without acknowledging or reopening it", async () => {
  const scope = new Scope({}), gate = deferred(), entered = deferred();
  const actions = [];
  scope.command = async action => {
    actions.push(action);
    if (action === "open") { entered.resolve(); await gate.promise; }
    return { code: 0, stdout: "{}" };
  };
  const opening = scope.open(process.cwd(), {});
  await entered.promise;
  const rejected = assert.rejects(opening, /closed/);
  const closing = scope.close();
  gate.resolve();
  await Promise.all([rejected, closing]);
  assert.deepEqual(actions, ["open", "close"]);
  await assert.rejects(scope.open(process.cwd(), {}), /reopened/);
});
