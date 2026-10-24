/** Expired real service handoffs must fail admission without poisoning the worker. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

export async function run({ createSession, recover, root, config }) {
  // Patch only the native fixture's ACK timing; admission and reclamation stay real.
  const { Scope } = await import(pathToFileURL(join(dirname(process.argv[1]), "scope.mjs")));
  const command = Scope.prototype.command;
  let expired;
  const cwd = join(root, "expired");
  mkdirSync(cwd);
  Scope.prototype.command = async function (action, ...args) {
    if (action === "acknowledge") {
      expired = this;
      const deadline = Date.now() + 7000;
      while (true) {
        const status = JSON.parse(execFileSync(config.executable,
          ["status", "--config", join(root, "aw.json")], { encoding: "utf8", timeout: 3000 })).result;
        if (status.bindings === 1) {
          assert.equal(status.events, 0);
          break;
        }
        assert(Date.now() < deadline, "Expired handoff was not reclaimed");
        await delay(50);
      }
    }
    return await command.call(this, action, ...args);
  };
  try {
    await assert.rejects(createSession({ cwd, index: 0,
      environment: { AW_TEST_MODE: "allow", AW_TEST_TENANT: "expired" } }),
    /AW task handoff acknowledgement failed/);
  } finally { Scope.prototype.command = command; }
  assert(expired.closed);
  assert.equal(expired.releaseError, null);
  assert(!existsSync(join(cwd, "agent")), "SDK factory ran before admission");
  await expired.close();
  const results = [{ case: "acknowledge-expired", passed: true }];
  const invalid = join(root, "invalid-stream");
  mkdirSync(invalid);
  await assert.rejects(createSession({ cwd: invalid, index: 0, invalidStream: true,
    environment: { AW_TEST_MODE: "allow", AW_TEST_TENANT: "invalid-stream" } }),
  /Pi requires its public model stream function/);
  const status = JSON.parse(execFileSync(config.executable,
    ["status", "--config", join(root, "aw.json")], { encoding: "utf8", timeout: 3000 })).result;
  assert.equal(status.bindings, 1);
  assert.equal(status.events, 0);
  results.push({ case: "invalid-native-stream-rejected", passed: true });
  await recover();
  results.push({ case: "worker-reuse", passed: true });
  return results;
}
