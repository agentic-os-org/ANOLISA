/** Prewarm through the public factory before updating and starting the first Query. */
import assert from "node:assert/strict";
import { existsSync, readFileSync, renameSync, watch, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { runFixture } from "@qoder-ai/qoder-agent-sdk";

const config = JSON.parse(readFileSync(process.env.AW_QODER_SDK_CONFIG));
const root = JSON.parse(readFileSync(config.binding));
const { createQuery, startDaemon } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const daemon = await startDaemon({});
try {
  assert.equal(JSON.parse(readFileSync(config.ready_path)).hooks, config.hooks.length);
  await new Promise((resolve, reject) => {
    const finish = error => {
      clearTimeout(timer);
      watcher.close();
      if (error) reject(error); else resolve();
    };
    const watcher = watch(".", () => {
      if (existsSync("release-first-query")) finish();
    });
    watcher.on("error", finish);
    const timer = setTimeout(() => finish(new Error("first Query gate timed out")), 10000);
    writeFileSync("first-query-gated.tmp", JSON.stringify({
      pid: process.pid, instance_id: root.binding.instance_id,
    }));
    renameSync("first-query-gated.tmp", "first-query-gated.json");
  });

  const query = await createQuery({ prompt: "fixture", options: {
    settingSources: [], cwd: process.cwd(), daemon,
  } });
  try {
    const result = await runFixture();
    const receipt = JSON.parse(readFileSync(config.ready_path));
    assert.equal(receipt.hooks, config.hooks.length, "readiness attests the launch configuration");
    process.stdout.write(JSON.stringify({ ...result, receipt_hooks: receipt.hooks }));
  } finally {
    await query.close();
    writeFileSync("query-closed", "yes");
  }

} finally {
  await daemon.stop();
}
