/** Exercise process-wide fencing through the assets installed by aw run. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
const bridge = pathToFileURL(process.env.AW_QODER_SDK_BRIDGE);
const { createQuery } = await import(bridge);
const { ExecutionOwner, PRIVATE_EXECUTION_VERSION } = await import(new URL("./private.mjs", bridge));
const query = await createQuery({ prompt: "fixture", options: { settingSources: [], cwd: process.cwd() } });
try {
  const failed = new ExecutionOwner(async () => ({ version: PRIVATE_EXECUTION_VERSION,
    environment: {}, hookEnvironment: {}, secrets: {}, redact: value => value,
    release: async () => { throw new Error("synthetic cleanup failure"); } }));
  await assert.rejects(failed.run({ invocationId: "fixture", toolName: "fixture", input: {} },
    async () => ({})), /restart/);
  await assert.rejects(failed.close(), /restart/);
  assert.throws(() => query.streamInput([]), /restart/);
  await assert.rejects(query.next(), /restart/);
  await assert.rejects(createQuery({ prompt: "late", options: { settingSources: [] } }), /restart/);
} finally {
  await query.close();
}
process.stdout.write(JSON.stringify({ fenced: true }));
