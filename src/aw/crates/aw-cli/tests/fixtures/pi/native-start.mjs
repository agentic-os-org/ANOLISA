/** Native Node host prepares its own bridge and keeps the same PID throughout. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

const aw = process.env.AW_TEST_EXECUTABLE;
function command(args) {
  const result = spawnSync(aw, args, { encoding: "utf8", timeout: 35000 });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim() ? JSON.parse(result.stdout) : null;
}
const prepared = command(["instance", "prepare", "--agent", "pi"]);
Object.assign(process.env, prepared.environment);
process.env.AW_TEST_INSTANCE = prepared.instance_id;
try {
  const { runNativeWorker } = await import(pathToFileURL(prepared.command.args[0]));
  await runNativeWorker({ application: prepared.command.args[1], args: prepared.command.args.slice(2) });
} finally {
  command(["instance", "release", "--binding", prepared.binding]);
}
