/** Published Pi tool execution and explicit policy replacement in one native host. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { createSession as nativeFactory } from "./resident-fixture.mjs";
export const nativeArguments = ["task with spaces", "--native-option=kept"];
export async function createSession(options) {
  assert.deepEqual(options.args, nativeArguments);
  return await nativeFactory(options);
}

export async function runWorker({ createSession, args }) {
  assert.deepEqual(args, nativeArguments);
  const root = process.env.FAKE_ROOT;
  const observations = [];
  const update = () => spawnSync(process.env.AW_TEST_EXECUTABLE,
    ["instance", "update", "--instance", process.env.AW_TEST_INSTANCE,
      "--policy", join(root, "deny.json")], { encoding: "utf8", timeout: 35000 });
  for (const index of [0, 1]) {
    const cwd = join(root, String(index));
    mkdirSync(cwd);
    const { session, close } = await createSession({ cwd, index });
    try {
      const busy = update();
      assert.notEqual(busy.status, 0);
      assert.match(busy.stderr, /busy/);
      await session.prompt("fixture");
      assert.equal(existsSync(join(cwd, "effect")), index === 0);
      const status = spawnSync(process.env.AW_TEST_EXECUTABLE, ["status", "--config", "aw.yaml"],
        { encoding: "utf8", timeout: 5000 });
      assert.equal(status.status, 0, status.stderr);
      const observation = JSON.parse(status.stdout).result.observations[0];
      assert.ok(observation, "ACK must start task-owned observation");
      assert.notEqual(observation.instance_id, process.env.AW_TEST_INSTANCE);
      assert.equal(observation.coverage, "unknown");
      if (index === 1) {
        assert.ok(observation.callbacks > 0, "native tool denial must retain its callback fact");
        assert.equal(observation.association.status, "unverified");
        assert.ok(observation.gaps.includes("native_identity_unverified"));
      }
      observations.push({ callbacks: observation.callbacks, coverage: observation.coverage,
        association: observation.association?.status ?? "absent" });
    } finally { await close(); }
    if (index === 0) {
      const changed = update();
      assert.equal(changed.status, 0, changed.stderr);
    }
  }
  writeFileSync(join(root, "native-result.json"), JSON.stringify({ passed: true,
    cases: ["native-start", "busy-rejected", "native-tool-effect", "updated-tool-denial", "session-cleanup", "task-observation-unknown"], observations }));
}
