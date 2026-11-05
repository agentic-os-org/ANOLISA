/** Real hosted Worker adopts task snapshots across new Queries in one daemon lifetime. */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";
import { z } from "zod";
const { createQuery, startDaemon, createSdkMcpServer } =
  await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const root = process.env.QODER_TEST_ROOT;
const config = JSON.parse(fs.readFileSync(process.env.AW_QODER_SDK_CONFIG));
const binding = JSON.parse(fs.readFileSync(config.binding));
assert.equal(config.resident_policy_reload, true);
const results = [];
let daemon, active;
const run = args => spawnSync(config.executable, args, { encoding: "utf8", timeout: 15000 });
function status() {
  const result = run(["service", "status", "--socket", binding.socket]);
  assert.equal(result.status, 0, result.stderr);
  return JSON.parse(result.stdout).result.instances.find(instance => instance.instance_id === binding.binding.instance_id);
}
function update(file, busy = false) {
  const before = status().policy_revision;
  const result = run(["update", "--socket", binding.socket, "--instance", binding.binding.instance_id,
    "--policy", path.join(root, file)]);
  if (busy) {
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /busy/);
    assert.equal(status().policy_revision, before);
  } else assert.equal(result.status, 0, result.stderr);
}
const events = () => fs.existsSync(path.join(root, "events.jsonl")) ?
  fs.readFileSync(path.join(root, "events.jsonl"), "utf8").trim().split("\n").filter(Boolean).map(JSON.parse) : [];
async function session(prompt, options = {}) {
  active = await createQuery({ prompt, options: { daemon, cwd: process.cwd(), settingSources: [],
    persistSession: false, permissionMode: "bypassPermissions", tools: ["Bash"],
    model: "auto", maxTurns: 3, ...options } });
  return active;
}
async function read(query) {
  const messages = [];
  let failure;
  const timer = setTimeout(() => query.close().catch(() => {}), 90000);
  try {
    for await (const message of query) {
      messages.push(message);
      if (message.type === "result") break;
    }
  } catch (error) { failure = error; }
  finally { clearTimeout(timer); await query.close(); active = null; }
  if (!failure) {
    const result = messages.findLast(message => message.type === "result");
    assert(result, "native Query did not complete a result");
    assert.equal(result.is_error, false, `native turn failed: ${result.subtype}`);
  }
  return { messages, failure };
}
async function bash(command) {
  return await session(`Use Bash exactly once with ${JSON.stringify({ command })}. ` +
    "If denied or failed, report it and stop. Do not retry or use another tool.");
}
const identities = () => fs.readFileSync(`/proc/${process.pid}/task/${process.pid}/children`, "utf8")
  .trim().split(/\s+/).filter(Boolean).map(pid => ({ pid: Number(pid),
    start: fs.readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1].split(" ")[19] }));
try {
  daemon = await startDaemon({ auth: { type: "accessToken",
    accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } },
    workerIdleTimeoutMs: 300000, debugFile: path.join(root, "daemon.log"), stderr: () => {} });
  const initial = identities();
  assert.equal(initial.length, 1);
  fs.writeFileSync(path.join(root, "daemon-process.json"), JSON.stringify({ identities: initial,
    cwd: process.cwd(), ports: [], log: path.join(root, "daemon.log"), deadline_seconds: 900,
    stop: `kill -TERM -- ${initial[0].pid}` }));
  // Stay prewarmed beyond the launcher's old first-Query receipt deadline.
  await new Promise(resolve => setTimeout(resolve, 35000));
  assert.equal(status().requests, 0);
  assert.equal(events().length, 0);
  update("candidate.json");
  let query = await bash("printf AW_DENY > denied.txt");
  assert.ifError((await read(query)).failure);
  assert(!fs.existsSync("denied.txt"));
  assert(events().some(event => event.name === "tool.before" && JSON.stringify(event.tool.input).includes("AW_DENY")));
  results.push({ case: "new-query-adds-security-hook", status: "passed" });
  update("empty.json");
  query = await bash("printf AW_ALLOW > initial.txt");
  update("candidate.json", true);
  assert.ifError((await read(query)).failure);
  assert(fs.existsSync("initial.txt"));
  results.push({ case: "open-query-busy-without-hooks", status: "passed" });
  const count = events().length;
  query = await bash("printf AW_DENY > removed.txt");
  assert.ifError((await read(query)).failure);
  assert(fs.existsSync("removed.txt"));
  assert.equal(events().length, count);
  results.push({ case: "new-query-removes-security-hook", status: "passed" });
  update("strict.json");
  query = await bash("printf AW_POST_FAIL");
  const withheld = await read(query);
  assert(withheld.failure, "updated strict Post must reach the native caller");
  const post = events().findLast(event => event.name === "tool.after");
  assert(post && JSON.stringify(post.tool.input).includes("AW_POST_FAIL"));
  assert(!withheld.messages.some(message => message.type === "user" && message.message?.content?.some(
    block => block.type === "tool_result" && block.tool_use_id === post.native.aw_native_tool_use_id)));
  results.push({ case: "new-query-enables-strict-post", status: "passed" });
  update("candidate.json");
  query = await bash("printf AW_POST_FAIL");
  assert.ifError((await read(query)).failure);
  results.push({ case: "new-query-restores-report-post", status: "passed" });
  let executions = 0;
  const server = await createSdkMcpServer({ name: "managed", tools: [{ name: "read", description: "Read the requested fixture",
    inputSchema: { marker: z.string() }, handler: async () => {
      executions++;
      return { content: [{ type: "text", text: "receipt" }] };
    } }] });
  query = await session("Call mcp__managed__read exactly once with marker AW_DENY. If denied, stop; do not retry.",
    { tools: [], mcpServers: { managed: server }, allowedTools: ["mcp__managed__read"] });
  assert.ifError((await read(query)).failure);
  assert.equal(executions, 0);
  assert(events().some(event => event.tool.native_name === "mcp__managed__read"));
  results.push({ case: "updated-managed-mcp-denial", status: "passed" });
  assert.equal(status().requests, 0);
  assert.deepEqual(identities(), initial);
  results.push({ case: "same-worker-and-daemon-across-revisions", status: "passed" });
  fs.writeFileSync(path.join(root, "results.json"), JSON.stringify(results));
} finally {
  await active?.close();
  await daemon?.stop();
  assert.equal(identities().length, 0);
}
