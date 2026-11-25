import assert from "node:assert/strict";
import { readFile, rm } from "node:fs/promises";
import { Instance } from "@anolisa/aw-sdk";
import net from "node:net";

const [endpoint, root, rounds] = process.argv.slice(2);
// Read-only probes share the same wire contract as the Python consumer.
async function probe(operation) {
  const identity = client.connection.identity;
  const request = Buffer.from(
    JSON.stringify({
      api_version: "aw-service/v1alpha7",
      identity,
      deadline_ns: Number(process.hrtime.bigint() + 2_000_000_000n),
      operation,
    }),
  );
  const header = Buffer.alloc(4);
  header.writeUInt32BE(request.length);
  return await new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint);
    let data = Buffer.alloc(0);
    socket.setTimeout(2000, () => socket.destroy(Error("probe timeout")));
    socket.on("error", reject);
    socket.on("connect", () => socket.write(Buffer.concat([header, request])));
    socket.on("data", (chunk) => {
      data = Buffer.concat([data, chunk]);
    });
    // Wait for the server to close the control connection after its reply.
    socket.on("end", () => {
      if (data.length < 4 || data.length !== 4 + data.readUInt32BE(0)) {
        reject(Error("incomplete AW probe response"));
        return;
      }
      const response = JSON.parse(data.subarray(4));
      if (response.error) reject(Error(response.error));
      else resolve(response.result);
    });
  });
}
async function waitForProvider(call, settled) {
  let earlyResult;
  settled.then((error) => {
    earlyResult = error ?? Error(`${call} completed before Provider startup`);
  });
  const deadline = performance.now() + 5000;
  while (performance.now() < deadline) {
    if (earlyResult) throw earlyResult;
    try {
      const pid = Number(await readFile(`${root}/event-pid`, "utf8"));
      assert.ok(Number.isSafeInteger(pid) && pid > 0, `invalid Provider PID for ${call}`);
      return pid;
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw Error(`${call} Provider startup timed out`);
}
const client = new Instance({
  endpoint,
  target: "app",
  availability: "required",
  timeoutMs: 2000,
  capabilities: {
    adapter: "sdk",
    version: "aw-hook-source/v1",
    entrypoint: "application",
    events: { "tool.before": ["observe", "block"] },
  },
  install: () => true,
});
const native = (call, input = {}) => ({
  api_version: "aw-hook-source/v1",
  event: "tool.before",
  session_id: "session",
  call_id: call,
  name: "tool",
  input,
});
try {
  await client.connect();
  const task = await client.task();
  const [first, second] = task.routes.hooks;
  await task.invoke(first, native("partial"));
  assert.equal(
    (await probe({ method: "status" })).resources.usage.event_slots,
    1,
  );
  await task.closeEvent(first, native("partial"));
  assert.equal(
    (await probe({ method: "status" })).resources.usage.event_slots,
    0,
  );
  await assert.rejects(
    task.invoke(second, native("partial")),
    /native_event_closed/,
  );
  await task.emit("tool.before", native("complete"));
  const retiredBy = performance.now() + 2000;
  while ((await probe({ method: "status" })).resources.usage.event_slots) {
    assert.ok(performance.now() < retiredBy, "completed event retirement");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  for (let attempt = 0; attempt < 2 * Number(rounds); attempt++) {
    const explicit = attempt % 2 === 0;
    const call = `${explicit ? "cancelled" : "aborted"}-${Math.floor(attempt / 2)}`;
    await rm(`${root}/event-pid`, { force: true });
    const abort = new AbortController();
    const pending = task.invoke(
      first,
      native(call, { slow: true }),
      abort.signal,
    );
    const settled = pending.then(
      () => null,
      (error) => error,
    );
    const pid = await waitForProvider(call, settled);
    assert.deepEqual(
      (await probe({ method: "sdk_control", command: { action: "list" } }))
        .applications,
      [],
    );
    const start = performance.now();
    // Explicit drain must work while the original RPC occupies the work limit.
    if (explicit) await task.closeEvent(first, native(call, { slow: true }));
    const cancelled = new Error("user_cancelled");
    abort.abort(cancelled);
    const error = await settled;
    assert.ok(error);
    if (!explicit) assert.equal(error, cancelled);
    assert.ok(performance.now() - start < 3000);
    assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
    assert.equal(
      (await probe({ method: "status" })).resources.usage.event_slots,
      0,
    );
    await task.emit("tool.before", native(`fresh-${call}`));
  }
  await rm(`${root}/event-pid`, { force: true });
  const pending = task
    .invoke(first, native("scope-close", { slow: true }))
    .then(
      () => null,
      (error) => error,
    );
  const pid = await waitForProvider("scope-close", pending);
  await task.close();
  assert.ok(await pending);
  assert.throws(() => process.kill(pid, 0), { code: "ESRCH" });
  const next = await client.task();
  await next.emit("tool.before", native("next-task"));
  await next.close();
} finally {
  await client.close();
}
