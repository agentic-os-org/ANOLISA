/** Direct and prewarmed application-owned Queries confirm the full root lifecycle. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { runFixture } from "@qoder-ai/qoder-agent-sdk";
const { createQuery, startup } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const config = JSON.parse(fs.readFileSync(process.env.AW_QODER_SDK_CONFIG));
const binding = JSON.parse(fs.readFileSync(config.binding));
const options = { cwd: process.cwd(), settingSources: [] };
function status() {
  const result = spawnSync(config.executable, ["status", "--socket", binding.socket],
    { encoding: "utf8", timeout: 10000 });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout).result;
}
function root() { return status().instances.find(value => value.instance_id === binding.binding.instance_id); }
assert.notEqual(root().ready, true);
const modes = process.env.QODER_PREWARM_FIRST ?
  ["prewarm", "direct", "unused-prewarm", "direct"] :
  ["direct", "prewarm", "unused-prewarm", "direct"];
for (const mode of modes) {
  let query, warm;
  try {
    if (mode === "direct") query = await createQuery({ prompt: "fixture", options });
    else {
      warm = await startup({ options });
      assert.equal(root().ready, true);
      query = mode === "prewarm" ? warm.query("fixture") : undefined;
    }
    assert.equal(root().ready, true);
    assert.equal(root().process.pid, process.pid);
    assert.equal(root().requests, 1);
    if (query) assert.equal((await runFixture()).denied, false);
  } finally {
    if (query) await query.close();
    if (warm) await warm.close();
  }
  assert.equal(root().requests, 0);
  assert.equal(status().bindings, 1);
  assert.equal(status().events, 0);
}
process.env.QODER_INIT_FAIL = "1";
await assert.rejects(createQuery({ prompt: "fixture", options }), /initialization failed/);
await assert.rejects(startup({ options }), /initialization failed/);
assert.equal(root().requests, 0);
assert.equal(status().bindings, 1);
assert.equal(status().events, 0);
process.stdout.write(JSON.stringify({ passed: true, direct: 2, prewarm: 2, initialization_failures: 2 }));
