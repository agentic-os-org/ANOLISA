/** Exercise the public bridge against a real AW service and explicit update CLI. */
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { setImmediate as tick } from "node:timers/promises";
import { pathToFileURL } from "node:url";
import { query, gate, gates, backgrounds } from "@qoder-ai/qoder-agent-sdk";
const { createQuery, startDaemon } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const config = JSON.parse(fs.readFileSync(process.env.AW_QODER_SDK_CONFIG));
const binding = JSON.parse(fs.readFileSync(config.binding));
const root = process.cwd();
const aw = (args) => spawnSync(config.executable, args, { encoding: "utf8", timeout: 15000 });
const singleConfig = process.env.QODER_SINGLE_CONFIG === "1";
const desired = path.join(root, "aw.json");
const initialBytes = fs.readFileSync(desired);
let serviceIdentity;
const state = () => {
  const endpoint = singleConfig ? ["--config", desired] : ["--socket", binding.socket];
  const result = aw(["service", "status", ...endpoint]);
  assert.equal(result.status, 0, result.stderr);
  const status = JSON.parse(result.stdout);
  serviceIdentity ??= status.identity;
  assert.deepEqual(status.identity, serviceIdentity);
  return status.result.instances[0];
};
function update(file, busy = false) {
  const previous = singleConfig && busy ? state().policy_revision : undefined;
  let args = ["update", "--socket", binding.socket, "--agent", "sdk", "--policy", path.join(root, file)];
  let candidate;
  if (singleConfig) {
    candidate = file === "aw.json" ? initialBytes : fs.readFileSync(path.join(root, file));
    const temporary = path.join(root, "editor.tmp");
    fs.writeFileSync(temporary, candidate, { mode: 0o600 });
    fs.renameSync(temporary, desired);
    args = ["update", "--config", desired, "--agent", "sdk"];
  }
  const result = aw(args);
  if (busy) {
    assert.notEqual(result.status, 0); assert.match(result.stderr, /busy/);
    if (singleConfig) assert.equal(state().policy_revision, previous);
  }
  else assert.equal(result.status, 0, result.stderr);
  if (singleConfig) assert.deepEqual(fs.readFileSync(desired), candidate);
}
function input() {
  let wake, ended = false;
  const messages = [];
  return {
    async *[Symbol.asyncIterator]() {
      while (!ended) {
        if (!messages.length) await new Promise((done) => { wake = done; });
        if (ended) return;
        const { text, uuid } = messages.shift();
        yield { type: "user", message: { role: "user", content: text }, parent_tool_use_id: null, ...(uuid ? { uuid } : {}) };
      }
    },
    send(text, uuid) { messages.push({ text, uuid }); wake?.(); },
    end() { ended = true; wake?.(); },
  };
}
async function result(q) {
  for (;;) { const item = await q.next(); assert(!item.done); if (item.value.type === "result") return item.value; }
}
const options = { daemon: await startDaemon({}), settingSources: [], cwd: root,
  agents: { foreground: { description: "foreground", prompt: "inspect", background: false } } };
const inputs = [input(), input()];
const sessions = await Promise.all(inputs.map((prompt) => createQuery({ prompt, options })));
if (process.env.QODER_RELOAD_FAILURE) {
  const brokenInput = input();
  const broken = await createQuery({ prompt: brokenInput, options: { ...options, failClose: true } });
  await assert.rejects(broken.return(), /native cleanup failed/);
  assert(state().requests > 0);
  update("candidate.json", true);
  brokenInput.end();
  for (const i of inputs) i.end();
  for (const q of sessions) await q.return();
  console.log(JSON.stringify({ passed: true }));
  process.exit(0);
}
const initial = state();
assert.equal(initial.ready, true);
assert.equal(initial.requests, 0);
update("candidate.json");
if (singleConfig) {
  for (const flags of [[], ["--reloadable"]]) {
    const result = aw(["run", "--config", desired, "--agent", "sdk", ...flags]);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /configuration path already selects another service/);
    assert.deepEqual(state().process, initial.process);
  }
}
inputs[0].send("first");
const first = await result(sessions[0]);
assert.equal(first.denied, true);
assert.equal(first.reason, "Retry with arguments: rtk candidate");
assert.equal(state().requests, 0);
update("native-failure.json");
inputs[0].send("native-failure");
assert.equal((await result(sessions[0])).denied, true);
update("native-report.json");
inputs[0].send("native-report");
assert.equal((await result(sessions[0])).denied, false);
// Removing all steps also changes this already-created Query, preserving its identity.
update("aw.json");
inputs[0].send("removed");
assert.equal((await result(sessions[0])).denied, false);
for (const command of ["background", "agent-background", "agent-worktree", "goal-create", "wakeup", "cron-create"]) {
  inputs[0].send(command);
  assert.equal((await result(sessions[0])).denied, true, command);
  assert(!fs.existsSync(`${command}.scheduled`), "native scheduling must not execute");
  assert.equal(state().requests, 0);
  update("aw.json");
}
for (const command of ["agent-foreground", "agent-default", "custom-worktree", "goal-update", "cron-list", "cron-delete"]) {
  inputs[0].send(command);
  assert.equal((await result(sessions[0])).denied, false, command);
}
// A normal Bash call can time out into native background work without an
// explicit background flag. Its notification turn also needs native cleanup.
for (const command of ["timeout-open", "timeout-finite"]) {
  const source = input();
  const q = await createQuery({ prompt: command === "timeout-finite" ? command : source, options });
  const cleanup = gate(`${command}:background-cleanup`);
  if (command === "timeout-open") source.send(command);
  assert.equal((await result(q)).denied, false);
  const { child, exited } = backgrounds.get(command);
  try {
    assert(fs.existsSync(`/proc/${child.pid}`));
    assert(state().requests > 0, "timeout background child must retain admission");
    for (let attempt = 0; attempt < 3; attempt++) {
      update("candidate.json", true);
      assert(fs.existsSync(`/proc/${child.pid}`));
      await tick();
    }
    child.stdin.end();
    await exited;
    assert(!fs.existsSync(`/proc/${child.pid}`));
    await tick();
    update("candidate.json", true);
    cleanup.resolve();
    assert.equal((await result(q)).autonomous, true);
    update("aw.json");
  } finally {
    child.stdin.end();
    cleanup.resolve();
    await exited;
    source.end();
    await q.close();
  }
}
// Native close alone leaves detached tasks running. Hold busy while the
// explicit native stop handshake waits for the child to exit.
{
  const source = input();
  const q = await createQuery({ prompt: source, options });
  source.send("timeout-close");
  await result(q);
  const { child, exited } = backgrounds.get("timeout-close");
  const entered = gate("stop-entered"), stopped = gate("stop-release");
  const closing = q.close();
  try {
    await entered.promise;
    assert(fs.existsSync(`/proc/${child.pid}`));
    update("candidate.json", true);
    stopped.resolve();
    await closing;
    assert(!fs.existsSync(`/proc/${child.pid}`));
    update("aw.json");
  } finally {
    stopped.resolve();
    child.stdin.end();
    await exited;
    source.end();
    await closing;
    gates.delete("stop-release");
  }
}
const submitted = gate("model:submitted"), model = gate("model:model");
inputs[1].send("model");
await submitted.promise;
assert.equal(state().requests, 1);
update("candidate.json", true);
model.resolve();
assert.equal((await result(sessions[1])).denied, false);
const started = gate("held:started"), tool = gate("held:tool"), cleanup = gate("held:cleanup");
const barrier = gate("held:barrier");
inputs[1].send("held");
await started.promise;
assert.equal(state().requests, 1);
update("candidate.json", true);
inputs[0].send("concurrent");
assert.equal((await result(sessions[0])).denied, false);
assert.equal(state().requests, 1, "another Query must not release the held request");
update("candidate.json", true);
tool.resolve();
await barrier.promise;
assert.equal(state().requests, 1, "native result must not release unfinished cleanup");
update("candidate.json", true);
cleanup.resolve();
assert.equal((await result(sessions[1])).denied, false);
update("candidate.json");
inputs[0].send("again");
assert.equal((await result(sessions[0])).denied, true);
assert.deepEqual(state().process, initial.process);
const closeEntered = gate("close-entered"), closeRelease = gate("close-release");
const uuidStarted = gate("uuid-close:started"), uuidTool = gate("uuid-close:tool");
inputs[0].send("uuid-close", "close");
await uuidStarted.promise;
const closing = sessions[0].return();
await Promise.race([closeEntered.promise, closing.then(() => { throw new Error("closed before native cleanup"); })]);
update("aw.json", true);
uuidTool.resolve();
closeRelease.resolve();
await closing;
gates.delete("close-release");
update("aw.json");
// An unmanaged Query on the same daemon does not acquire AW admission or Hooks.
const unmanagedInput = input();
const unmanaged = query({ prompt: unmanagedInput, options: { ...options, hooks: {} } });
unmanagedInput.send("unmanaged");
assert.equal((await result(unmanaged)).denied, false);
assert.equal(state().requests, 0);
unmanagedInput.end(); await unmanaged.return();
for (const i of inputs) i.end();
await sessions[1].close();
assert.equal(state().requests, 0);
const finite = await createQuery({ prompt: "finite", options });
// Let the native reader finish before the caller drains its buffered messages.
await new Promise((resolve) => setTimeout(resolve, 40));
const completed = [];
for await (const message of finite) completed.push(message);
assert(completed.some((message) => message.type === "result"));
assert.equal(state().requests, 0);
// Prefetch reads must stay ordered and all settle when the native stream ends
// or the caller closes it, even after an earlier message wakes every reader.
for (const termination of ["end", "close"]) {
  const source = input();
  const q = await createQuery({ prompt: source, options });
  const reads = Array.from({ length: 5 }, () => q.next());
  for (const sequence of [0, 1, 2]) {
    q.emit({ type: "system", sequence });
    await tick();
  }
  if (termination === "end") source.end();
  else await q.close();
  let timeout;
  try {
    const received = await Promise.race([
      Promise.all(reads),
      new Promise((_, reject) => { timeout = setTimeout(() => reject(new Error(`${termination}: concurrent reads did not settle`)), 2000); }),
    ]);
    assert.deepEqual(received.slice(0, 3).map((item) => item.value.sequence), [0, 1, 2]);
    assert(received.slice(0, 3).every((item) => !item.done));
    assert(received.slice(3).every((item) => item.done));
    assert.equal((await q.next()).done, true);
  } finally {
    clearTimeout(timeout);
    source.end();
    await q.close();
  }
}
assert.equal(state().requests, 0);
console.log(JSON.stringify({ passed: true }));
