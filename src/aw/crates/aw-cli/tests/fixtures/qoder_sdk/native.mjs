/** Real startDaemon()/query() tool turns against the pinned SDK and bundled Worker. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { query } from "@qoder-ai/qoder-agent-sdk";
const { createQuery, startDaemon } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const root = process.env.QODER_TEST_ROOT;
const results = [];
const queries = [];
let daemon;
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
async function turn(s, command, marker, expected) {
  s.send(`Use Bash exactly once to execute this exact command: ${command}. If denied or failed, report it and stop; do not retry or use another tool.`);
  let completed = false;
  const timeout = setTimeout(() => s.q.return(), 90000);
  try {
    for (;;) {
      const next = await s.q.next();
      if (next.done) break;
      if (next.value.type === "result") {
        assert.equal(next.value.is_error, false, `native turn failed: ${next.value.subtype}`);
        completed = true;
        break;
      }
    }
  } finally { clearTimeout(timeout); }
  assert(completed, "native turn did not complete within its deadline");
  assert.equal(fs.existsSync(marker), expected, command);
}
try {
  daemon = await startDaemon({ auth: { type: "accessToken", accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } },
    workerIdleTimeoutMs: 300000, env: { ...process.env, QODER_AGENT_SDK_DAEMON_CAPACITY: "10" },
    debugFile: path.join(root, "daemon.log"), stderr: () => {} });
  const pids = children().map(identity);
  assert.equal(pids.length, 1, "expected one owned SDK daemon");
  fs.writeFileSync(path.join(root, "daemon-process.json"), JSON.stringify({ identities: pids,
    command: fs.readFileSync(`/proc/${pids[0].pid}/cmdline`, "utf8").split("\0").filter(Boolean), cwd: process.cwd(), ports: [],
    log: path.join(root, "daemon.log"), deadline_seconds: 480,
    stop: `kill -TERM -- ${pids[0].pid}` }));
  const managed = await session(true);
  await daemon.getModels({ fetchStrategy: "live" });
  const unbound = await session(false);
  await turn(managed, "printf AW_DENY > denied.txt", "denied.txt", false);
  const events = () => fs.readFileSync(path.join(root, "events.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
  const first = events();
  assert(first.some((event) => event.name === "tool.before" && JSON.stringify(event.tool.input).includes("AW_DENY")), "denial must come from AW");
  results.push({ case: "first-request-denied", status: "passed" });
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
  await turn(failure, "exit 7", "failed-marker.txt", false);
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
  assert.deepEqual(pids.map(({ pid }) => identity(pid)), pids);
  results.push({ case: "daemon-not-restarted", status: "passed" });
  fs.writeFileSync(path.join(root, "results.json"), JSON.stringify(results));
} finally {
  for (const item of queries) { item.end(); await item.q.return(); }
  if (daemon) await daemon.stop();
  assert.equal(children().length, 0, "owned SDK daemon remains after stop");
}
