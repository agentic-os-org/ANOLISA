/** Application-owned startup reuses the admitted resident factory without an AW launcher. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const prepared = spawnSync(process.env.AW_TEST_EXECUTABLE, [
  "instance", "prepare", "--config", process.env.AW_TEST_CONFIG, "--agent", "sdk",
], { encoding: "utf8", timeout: 30000, maxBuffer: 1048576 });
assert.equal(prepared.status, 0, prepared.stderr);
const registration = JSON.parse(prepared.stdout);
assert.equal(typeof registration.instance_id, "string");
try {
  const inspected = spawnSync(process.env.AW_TEST_EXECUTABLE,
    ["status", "--config", process.env.AW_TEST_CONFIG],
    { encoding: "utf8", timeout: 10000 });
  assert.equal(inspected.status, 0, inspected.stderr);
  const pending = JSON.parse(inspected.stdout).result.instances.find(
    instance => instance.instance_id === registration.instance_id);
  assert(pending, "application registration must be visible before native initialization");
  assert.notEqual(pending.ready, true, "preparation must not acknowledge native readiness");
  Object.assign(process.env, registration.environment);
  await import(pathToFileURL(process.env.AW_TEST_WORKER));
} finally {
  const released = spawnSync(process.env.AW_TEST_EXECUTABLE,
    ["instance", "release", "--binding", registration.binding],
    { encoding: "utf8", timeout: 10000 });
  assert.equal(released.status, 0, released.stderr);
}
