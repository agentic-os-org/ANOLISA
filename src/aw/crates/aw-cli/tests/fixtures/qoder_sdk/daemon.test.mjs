/** Credential replacement keeps every original callback owned until daemon shutdown. */
import test from "node:test";
import assert from "node:assert/strict";
import { managedDaemon, daemonOwner } from "../../../../aw-adapter/native/qoder-sdk/daemon.mjs";
import { bounded } from "../../../../aw-adapter/native/qoder-sdk/resident.mjs";

const pause = () => new Promise(resolve => setImmediate(resolve));
const deferred = () => {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
};

async function fixture(auth, deadline = bounded) {
  const state = { calls: 0, stops: 0, payloads: [], events: [] };
  const config = { binding: "fixture-binding" };
  const daemon = await managedDaemon({ async startDaemon(options) {
    state.held = options.auth;
    return {
      async auth(payload) {
        state.calls++;
        if (payload !== undefined) state.held = payload;
        state.payloads.push(state.held);
        const callback = state.held?.fetchJobToken ?? state.held?.fetchServiceAccountToken;
        if (!callback) return false;
        await callback({ reason: "fixture" });
        return true;
      },
      async stop() { state.stops++; state.events.push("native-stop"); },
    };
  } }, { auth }, config, deadline);
  return { daemon, state, record: daemonOwner(daemon, config) };
}

for (const name of ["fetchJobToken", "fetchServiceAccountToken"]) {
  for (const replace of [false, true]) {
    test(`${name} ${replace ? "replacement" : "startup"} finalizer delays daemon stop`, { timeout: 5000 }, async () => {
      const entered = deferred(), release = deferred();
      const failure = new Error("fixture credential refused");
      let finalized = false;
      const payload = { type: name === "fetchJobToken" ? "jobToken" : "serviceAccount",
        [name]: async input => {
          assert.deepEqual(input, { reason: "fixture" });
          entered.resolve();
          try { await release.promise; throw failure; } finally { finalized = true; }
        } };
      const f = await fixture(replace ? undefined : payload);
      const authenticating = (replace ? f.daemon.auth(payload) : f.daemon.auth())
        .then(() => assert.fail("native credential failure must propagate"), error => error);
      let stopping, stopped = false;
      try {
        await entered.promise;
        assert.equal(f.record.callbacks.size, 1);
        stopping = f.daemon.stop().then(() => { stopped = true; });
        await pause();
        assert.equal(stopped, false);
        assert.equal(f.state.stops, 0);
        assert.equal(finalized, false);
        for (const replacement of [undefined, payload]) {
          const rejected = f.daemon.auth(replacement).then(() => null, error => error);
          await pause();
          assert.equal(f.state.calls, 1, "closing must reject before native auth is called");
          assert.match((await rejected)?.message ?? "", /closing/);
        }
      } finally {
        release.resolve();
        assert.equal(await authenticating, failure);
        await (stopping ?? f.daemon.stop());
      }
      assert(finalized);
      assert.equal(f.record.callbacks.size, 0);
      assert.equal(f.state.stops, 1);
    });
  }
}

test("replacement snapshots payloads and joins callbacks from every credential generation", { timeout: 5000 }, async () => {
  const oldEntered = deferred(), oldRelease = deferred(), newEntered = deferred(), newRelease = deferred();
  const old = { type: "jobToken", fetchJobToken: async () => { oldEntered.resolve(); await oldRelease.promise; } };
  const f = await fixture(old);
  const first = f.daemon.auth();
  await oldEntered.promise;
  const current = { type: "serviceAccount", nested: { region: "fixture" },
    fetchServiceAccountToken: async () => { newEntered.resolve(); await newRelease.promise; } };
  const second = f.daemon.auth(current);
  current.fetchServiceAccountToken = () => assert.fail("caller mutation must not replace the tracked callback");
  current.nested.region = "changed";
  await newEntered.promise;
  let stopped = false;
  const stopping = f.daemon.stop().then(() => { stopped = true; });
  try {
    assert.equal(f.record.callbacks.size, 2);
    assert.equal(f.state.held.nested.region, "fixture");
    newRelease.resolve();
    assert.equal(await second, true);
    await pause();
    assert.equal(stopped, false, "the previous credential generation still owns its finalizer");
    assert.equal(f.record.callbacks.size, 1);
  } finally {
    oldRelease.resolve();
    newRelease.resolve();
    await Promise.all([first, second, stopping]);
  }
  assert.equal(f.state.stops, 1);
  assert.equal(f.record.callbacks.size, 0);
});

test("credential drain timeout stops native daemon and reports incomplete cleanup", { timeout: 5000 }, async () => {
  const entered = deferred(), release = deferred();
  const f = await fixture(undefined, promise => bounded(promise, 20));
  const authenticating = f.daemon.auth({ type: "jobToken", fetchJobToken: async () => {
    entered.resolve();
    await release.promise;
  } });
  await entered.promise;
  try {
    await assert.rejects(f.daemon.stop(), error => error instanceof AggregateError &&
      error.errors.some(cause => /cleanup deadline/.test(cause.message)));
    assert.equal(f.state.stops, 1);
    assert.equal(f.record.callbacks.size, 1, "deadline failure must not declare the callback drained");
  } finally {
    release.resolve();
    await authenticating;
    await f.daemon.stop().catch(() => {});
  }
  assert.equal(f.record.callbacks.size, 0);
});
