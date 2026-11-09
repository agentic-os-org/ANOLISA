/** Pinned native initialization and cleanup, with no prompt or hosted model request. */
import assert from "node:assert/strict";
import { readFileSync, writeFileSync, existsSync, readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
const root = process.env.QODER_TEST_ROOT;
const { createQuery, startup } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const config = JSON.parse(readFileSync(process.env.AW_QODER_SDK_CONFIG));
const binding = JSON.parse(readFileSync(config.binding));
const options = { cwd: process.cwd(), settingSources: [], persistSession: false,
  permissionMode: "default", tools: [],
  auth: { type: "accessToken", accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } } };
const run = args => spawnSync(config.executable, args, { encoding: "utf8", timeout: 15000 });
function status() {
  const reply = run(["status", "--socket", binding.socket]);
  assert.equal(reply.status, 0, reply.stderr);
  return JSON.parse(reply.stdout).result;
}
const instance = () => status().instances.find(value => value.instance_id === binding.binding.instance_id);
const identities = () => [...new Set(readdirSync(`/proc/${process.pid}/task`).flatMap(tid => {
  try { return readFileSync(`/proc/${process.pid}/task/${tid}/children`, "utf8").trim().split(/\s+/).filter(Boolean); }
  catch (error) { if (error.code === "ENOENT") return []; throw error; }
}))].map(pid => ({ pid: Number(pid),
  start_ticks: readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1].split(" ")[19] }));
// The pinned SDK defaults to WorkerTransport: its official runtime executes in
// a Node worker thread. Keep only thread IDs from Node's diagnostic report.
const workers = () => process.report.getReport().workers.map(worker => worker.header.threadId);
const records = [];
assert.notEqual(instance().ready, true);
for (const mode of ["direct-first", "prewarm", "direct-reuse", "unused-prewarm"]) {
  let query, warm, activate;
  const gate = new Promise(resolve => { activate = resolve; });
  const input = async function* () { await gate; };
  let children = [], nativeWorkers = [];
  const previousWorkers = new Set(workers());
  try {
    if (mode.startsWith("direct")) query = await createQuery({ prompt: input(), options }, { initializeTimeoutMs: 60000 });
    else warm = await startup({ options, initializeTimeoutMs: 60000 });
    assert.equal(instance().ready, true);
    assert.equal(instance().process.pid, process.pid);
    assert.equal(instance().requests, 1);
    children = identities();
    nativeWorkers = workers().filter(id => !previousWorkers.has(id));
    assert.equal(nativeWorkers.length, 1, "native Query did not initialize its bundled Worker thread");
    const busy = run(["instance", "update",
      "--instance", binding.binding.instance_id, "--config", process.env.AW_TEST_CONFIG]);
    assert.notEqual(busy.status, 0);
    assert.match(busy.stderr, /busy/);
    if (mode === "prewarm") query = warm.query(input());
  } finally {
    activate();
    if (query) await query.close();
    if (warm) await warm.close();
  }
  for (const child of children)
    assert(!existsSync(`/proc/${child.pid}`), "native Worker survived Query close");
  assert(nativeWorkers.every(id => !workers().includes(id)), "native Worker thread survived Query close");
  assert.equal(instance().requests, 0);
  assert.equal(status().bindings, 1);
  assert.equal(status().events, 0);
  const updated = run(["instance", "update",
    "--instance", binding.binding.instance_id, "--config", process.env.AW_TEST_CONFIG]);
  assert.equal(updated.status, 0, updated.stderr);
  records.push({ case: mode, passed: true, workers_stopped: nativeWorkers.length, child_processes_stopped: children.length });
}
writeFileSync(join(root, "results.json"), JSON.stringify({ passed: true, sdk: "1.0.56",
  worker: "1.1.78", prompt_submissions: 0, daemon: false, cases: records }));
