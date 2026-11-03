/** A process cleanup fence must survive every asynchronous Query admission gate. */
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { fixture, deferred } from "./resident-support.mjs";
import { ExecutionOwner, PRIVATE_EXECUTION_VERSION } from "../../../../aw-adapter/native/qoder-sdk/private.mjs";

async function scenario(phase) {
  const cleanup = [];
  const f = fixture({ after: close => cleanup.push(close) }, phase === "handshake");
  const entered = deferred(), release = deferred();
  let result, callback;
  try {
    if (phase !== "import") f.state.importGate.resolve();
    const creating = f.create();
    if (phase === "import") await f.state.imported.promise;
    else if (phase === "handshake") await f.state.initializationEntered.promise;
    else {
      result = await creating;
      assert.ifError(result.error);
      if (phase === "callback") {
        f.resident.identities.claim = async () => ({ phase: "issued" });
        const hooks = f.resident.decorate({ hooks: { PreToolUse: [{ hooks: [async () => {
          entered.resolve();
          await release.promise;
          return {};
        }] }] } }).hooks;
        callback = hooks.PreToolUse[0].hooks[0]({}, "native", {});
        await entered.promise;
      }
    }
    const poisoned = new ExecutionOwner(async () => ({ version: PRIVATE_EXECUTION_VERSION,
      environment: {}, hookEnvironment: {}, secrets: {}, redact: value => value,
      release: async () => { throw new Error("synthetic release failure"); } }));
    await assert.rejects(poisoned.run({ invocationId: "poison", toolName: "fixture", input: {} },
      async () => ({})), /release failed/);
    await assert.rejects(poisoned.close(), /release failed/);
    f.state.closeGate.resolve();
    f.state.importGate.resolve();
    f.state.initializationGate.resolve();
    release.resolve();
    if (!result) {
      result = await creating;
      assert.match(String(result.error), /restart/);
      assert.equal(f.state.ready, 0);
      assert.equal(f.state.created, phase === "import" ? 0 : 1);
      await f.state.dispatch;
      assert.equal(f.state.executions, 0);
      assert.equal(f.state.scopeCloses, 1);
    } else {
      assert.throws(() => f.scope.checkExecution(), /restart/);
      assert.throws(() => result.value.streamInput([]), /restart/);
      assert.throws(() => f.resident.identities.ticket({}), /restart/);
      if (callback)
        assert.equal((await callback).hookSpecificOutput.permissionDecision, "deny");
      await assert.rejects(result.value.next(), /restart/);
      assert.equal(f.state.scopeCloses, 1);
    }
  } finally {
    release.resolve();
    for (const close of cleanup) await close();
  }
}

if (process.argv[2]) {
  await scenario(process.argv[2]);
} else {
  for (const phase of ["import", "handshake", "ready", "callback"]) {
    test(`process cleanup failure fences Query ${phase}`, () => {
      // Each process owns an irreversible fence; never reset production state for a test.
      const result = spawnSync(process.execPath, [fileURLToPath(import.meta.url), phase],
        { encoding: "utf8", timeout: 6000 });
      assert.equal(result.status, 0, `${result.stdout}\n${result.stderr}\n${result.error ?? ""}`);
    });
  }
}
