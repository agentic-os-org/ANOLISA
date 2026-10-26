/** Real startDaemon()/query() tool turns against the pinned SDK and bundled Worker. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { query } from "@qoder-ai/qoder-agent-sdk";
import { safety } from "./native-safety.mjs";
const { createQuery, startDaemon } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const root = process.env.QODER_TEST_ROOT;
const results = [];
const queries = [];
let daemon;
const reloadable = process.env.QODER_RELOAD_ACCEPTANCE === "1";
const awConfig = JSON.parse(fs.readFileSync(process.env.AW_QODER_SDK_CONFIG));
const awBinding = JSON.parse(fs.readFileSync(awConfig.binding));
function update(file, busy = false) {
  const result = spawnSync(awConfig.executable, ["update", "--socket", awBinding.socket,
    "--agent", "sdk", "--policy", path.join(root, file)], { encoding: "utf8", timeout: 15000 });
  if (busy) { assert.notEqual(result.status, 0); assert.match(result.stderr, /busy/); }
  else assert.equal(result.status, 0, result.stderr);
}
function state() {
  const result = spawnSync(awConfig.executable, ["service", "status", "--socket", awBinding.socket],
    { encoding: "utf8", timeout: 5000 });
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout).result.instances[0];
}
async function idle() {
  const deadline = Date.now() + 15000;
  for (;;) {
    const active = state();
    if (active.requests === 0 && active.tools === 0) return;
    assert(Date.now() < deadline, `SDK did not become idle: ${JSON.stringify(active)}`);
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
function bounded(promise, label) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`${label} deadline`)), 30000);
  })]).finally(() => clearTimeout(timer));
}
const children = () => fs.readFileSync(`/proc/${process.pid}/task/${process.pid}/children`, "utf8").trim().split(/\s+/).filter(Boolean);
const identity = (pid) => ({ pid: Number(pid), start: fs.readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1].split(" ")[19] });
async function session(managed, options = {}) {
  let release;
  const messages = [];
  let ended = false;
  async function* input() {
    while (!ended) {
      if (!messages.length) await new Promise((resolve) => { release = resolve; });
      if (ended) return;
      yield { type: "user", message: { role: "user", content: messages.shift() }, parent_tool_use_id: null };
    }
  }
  const args = { prompt: input(), options: { daemon, cwd: process.cwd(), settingSources: [],
    persistSession: false, model: "auto", maxTurns: 3, tools: ["Bash"],
    permissionMode: "bypassPermissions", ...options } };
  const q = managed ? await createQuery(args) : query(args);
  await q.initializationResult();
  queries.push({ q, end() { ended = true; release?.(); } });
  return { q, send(text) { messages.push(text); release?.(); } };
}
async function prompt(s, text) {
  s.send(text);
  const messages = [];
  let completed = false;
  const timeout = setTimeout(() => s.q.return(), 90000);
  try {
    for (;;) {
      const next = await s.q.next();
      if (next.done) break;
      messages.push(next.value);
      if (next.value.type === "result") {
        assert.equal(next.value.is_error, false, `native turn failed: ${next.value.subtype}`);
        completed = true;
        break;
      }
    }
  } finally { clearTimeout(timeout); }
  assert(completed, "native turn did not complete within its deadline");
  if (reloadable) await idle();
  return messages;
}
async function turn(s, command, marker, expected) {
  const messages = await prompt(s, `Use Bash exactly once to execute this exact command: ${command}. If denied or failed, report it and stop; do not retry or use another tool.`);
  assert.equal(fs.existsSync(marker), expected, command);
  return messages;
}
try {
  daemon = await startDaemon({ auth: { type: "accessToken", accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } },
    workerIdleTimeoutMs: 300000, env: { ...process.env, QODER_AGENT_SDK_DAEMON_CAPACITY: "10" },
    debugFile: path.join(root, "daemon.log"), stderr: () => {} });
  const pids = children().map(identity);
  assert.equal(pids.length, 1, "expected one owned SDK daemon");
  fs.writeFileSync(path.join(root, "daemon-process.json"), JSON.stringify({ identities: pids,
    command: fs.readFileSync(`/proc/${pids[0].pid}/cmdline`, "utf8").split("\0").filter(Boolean), cwd: process.cwd(), ports: [],
    log: path.join(root, "daemon.log"), deadline_seconds: 900,
    stop: `kill -TERM -- ${pids[0].pid}` }));
  const managed = await session(true);
  await daemon.getModels({ fetchStrategy: "live" });
  const unbound = await session(false);
  if (reloadable) update("candidate.json");
  await turn(managed, "printf AW_DENY > denied.txt", "denied.txt", false);
  const events = () => fs.readFileSync(path.join(root, "events.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
  const first = events();
  assert(first.some((event) => event.name === "tool.before" && JSON.stringify(event.tool.input).includes("AW_DENY")), "denial must come from AW");
  results.push({ case: "first-request-denied", status: "passed" });
  if (!reloadable) {
    const before = first.find(event => event.name === "tool.before");
    assert.match(before.tool.call_id, /^[0-9a-f-]{36}$/);
    assert.equal(typeof before.native.aw_native_tool_use_id, "string");
    assert.notEqual(before.tool.call_id, before.native.aw_native_tool_use_id);
    assert.equal(managed.q.handleControlRequest, undefined);
    assert.equal(managed.q.recordInboundSessionMessage, undefined);
    assert.equal(managed.q.transport, undefined);
    results.push({ case: "native-producer-identity", status: "passed" });
  }
  const count = first.length;
  await turn(unbound, "printf AW_DENY > unbound.txt", "unbound.txt", true);
  assert.equal(events().length, count, "AW hooks leaked to the unbound session");
  results.push({ case: "session-isolation", status: "passed" });
  const allowed = await session(true);
  await turn(allowed, "printf AW_ALLOW > allowed.txt", "allowed.txt", true);
  const after = events().find((event) => event.name === "tool.after");
  assert(after, "successful tool has no AW after event");
  assert(events().some((event) => event.name === "tool.before" && event.session_id === after.session_id && event.tool.call_id === after.tool.call_id));
  results.push({ case: "allow-before-after", status: "passed" });
  const failure = await session(true);
  await turn(failure, "sh -c 'exit 7'", "failed-marker.txt", false);
  assert(events().some((event) => event.native.hook_event_name === "PostToolUseFailure" && event.tool.result.is_error === true));
  results.push({ case: "tool-failure-observed", status: "passed" });
  let permissions = 0;
  const veto = await session(true, { permissionMode: "default", canUseTool: async () => {
    permissions++;
    return { behavior: "deny", message: "Native acceptance veto; do not retry" };
  } });
  await turn(veto, "printf NATIVE_VETO > native-veto.txt", "native-veto.txt", false);
  assert(permissions > 0, "the native permission callback was not exercised");
  results.push({ case: "native-permission-veto", status: "passed" });
  if (reloadable) {
    const originalSession = first.find((event) => event.name === "tool.before").session_id;
    update("aw.json");
    await turn(managed, "printf AW_DENY > removed.txt", "removed.txt", true);
    update("candidate.json");
    await turn(managed, "printf AW_DENY > readded.txt", "readded.txt", false);
    assert(events().some((event) => event.session_id === originalSession &&
      JSON.stringify(event.tool.input).includes("readded.txt")));
    results.push({ case: "existing-session-policy-update", status: "passed" });
    let entered, release;
    const pendingPermission = new Promise((resolve) => { entered = resolve; });
    const cleanup = new Promise((resolve) => { release = resolve; });
    const held = await session(true, { permissionMode: "default", canUseTool: async () => {
      entered();
      await cleanup;
      return { behavior: "deny", message: "Native cleanup regression veto" };
    } });
    held.send("Use Bash exactly once to run: printf NATIVE_HOLD > held.txt. Do not retry.");
    try {
      await bounded(pendingPermission, "native permission");
      update("aw.json", true);
      results.push({ case: "other-session-busy-update", status: "passed" });
      const closing = held.q.return();
      // The native callback is still held even if the SDK aborts its request.
      await new Promise((resolve) => setTimeout(resolve, 30));
      update("aw.json", true);
      release();
      await bounded(closing, "native close");
      update("aw.json");
      assert(!fs.existsSync("held.txt"));
      results.push({ case: "native-cleanup-busy-until-closed", status: "passed" });
    } finally { release(); }
  }
  if (reloadable) update("candidate.json");
  await safety({ session, turn, prompt, results, root, reloadable, update, state });
  assert.deepEqual(pids.map(({ pid }) => identity(pid)), pids);
  results.push({ case: "daemon-not-restarted", status: "passed" });
  fs.writeFileSync(path.join(root, "results.json"), JSON.stringify(results));
} finally {
  for (const item of queries) { item.end(); await item.q.return(); }
  if (daemon) await daemon.stop();
  assert.equal(children().length, 0, "owned SDK daemon remains after stop");
}
