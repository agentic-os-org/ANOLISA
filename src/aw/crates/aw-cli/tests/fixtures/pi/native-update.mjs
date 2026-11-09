/** Real AW policy transactions around deterministic public SDK task boundaries. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const root = process.env.FAKE_ROOT;
let calls = 0;
let aborts = 0;
let disposals = 0;
export async function createSession({ sdk, cwd, args }) {
  assert.deepEqual(args, ["task with spaces", "--native-option=kept"]);
  const duringPreparation = spawnSync(process.env.AW_TEST_EXECUTABLE,
    ["instance", "update", "--instance", process.env.AW_TEST_INSTANCE,
      "--policy", join(root, "deny.json")], { encoding: "utf8", timeout: 35000 });
  assert.notEqual(duringPreparation.status, 0, "factory preparation must retain occupancy");
  assert.match(duringPreparation.stderr, /busy/);
  const session = new sdk.AgentSession(cwd);
  session.agent.streamFunction = async () => { calls++; };
  const abort = session.abort.bind(session), dispose = session.dispose.bind(session);
  session.abort = async () => { aborts++; await abort(); };
  session.dispose = () => { disposals++; return dispose(); };
  return { session };
}
export async function runWorker({ createSession, args }) {
  assert.deepEqual(args, ["task with spaces", "--native-option=kept"]);
  const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
  const binding = JSON.parse(readFileSync(config.binding, "utf8"));
  const update = (name = "deny.json") => spawnSync(config.executable,
    ["instance", "update", "--instance", process.env.AW_TEST_INSTANCE,
      "--policy", join(root, name)], { encoding: "utf8", timeout: 35000 });
  if (process.env.PI_REJECT_STRICT === "1") {
    const unsupported = update("strict.json");
    assert.notEqual(unsupported.status, 0, "unsupported strict output must reject activation");
    assert.match(unsupported.stderr, /failure|unsupported|withhold/);
  }
  const first = await createSession({ cwd: root });
  const busy = update();
  assert.notEqual(busy.status, 0, "active session must retain its policy");
  assert.match(busy.stderr, /busy/);
  await first.session.prompt("risk");
  assert.equal(calls, 1, "empty initial policy must allow dispatch");
  await first.close();
  const changed = update();
  assert.equal(changed.status, 0, changed.stderr);
  const { Scope } = await import(pathToFileURL(join(dirname(process.env.AW_NATIVE_CONFIG), "scope.mjs")));
  const acknowledge = Scope.prototype.acknowledge;
  Scope.prototype.acknowledge = async () => { throw new Error("fixture acknowledgement failure"); };
  try {
    await assert.rejects(createSession({ cwd: root }), /fixture acknowledgement failure/);
    assert.equal(aborts, 2, "failed acknowledgement must abort the native session");
    assert.equal(disposals, 2, "failed acknowledgement must dispose the native session");
  } finally { Scope.prototype.acknowledge = acknowledge; }
  assert.equal(update().status, 0, "confirmed failed-start cleanup must release occupancy");
  const second = await createSession({ cwd: root });
  await assert.rejects(second.session.prompt("risk"), /AW|blocked/);
  assert.equal(calls, 1, "updated denial must precede model dispatch");
  await second.session.prompt("safe");
  assert.equal(calls, 2);
  await second.close();
  writeFileSync(join(root, "native-result.json"), JSON.stringify({
    passed: true, pid: process.pid, instance_id: binding.instance_id,
  }));
}
