/** Close during a paused native tool turn must fence the SDK's post-abort model attempt. */
import assert from "node:assert/strict";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

async function bounded(operation, milliseconds = 8000) {
  let timer;
  try {
    return await Promise.race([operation, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("Pi cancellation test exceeded its deadline")), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

async function cancel({ createSession, root, evidence }, after, direct = false) {
  const name = `${direct ? "abort" : "cancel"}-${after ? "after" : "before"}-tool`;
  const cwd = join(root, name);
  mkdirSync(cwd);
  const { session, close } = await createSession({ cwd, index: 0,
    environment: { AW_TEST_MODE: "allow", AW_TEST_TENANT: "cancelled" } });
  let enter, resume;
  const entered = new Promise((resolve) => { enter = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  let paused = false;
  const unsubscribe = session.agent.subscribe(async (event) => {
    if (!paused && (after ? event.type === "turn_end"
      : event.type === "message_end" && event.message.role === "assistant")) {
      paused = true;
      enter();
      await released;
    }
  });
  // Observe the rejection immediately so cleanup cannot leave an unhandled prompt.
  const prompting = session.prompt("cancel a native tool turn").then(
    () => ({ rejected: false }), (error) => ({ rejected: true, error: String(error) }));
  let closing;
  try {
    await bounded(entered);
    closing = direct ? session.abort() : close();
    assert(session.agent.signal.aborted, "close did not abort the native SDK");
    resume();
    const result = await bounded(prompting);
    await bounded(closing);
    const stats = evidence.get(session);
    writeFileSync(join(cwd, "requests.json"), JSON.stringify(stats.requests));
    assert.equal(stats.requests.length, 1, "Model transport ran after cancellation");
    assert.equal(stats.before, after ? 1 : 0);
    assert.equal(stats.after, after ? 1 : 0);
    assert.equal(existsSync(join(cwd, "effect")), after, "Unexpected tool execution");
    assert.equal(existsSync(join(cwd, "events.jsonl")), after, "Unexpected Provider invocation");
    assert(result.rejected, "Cancelled prompt returned success");
    await assert.rejects(session.prompt("closed task"), /closed/);
    await close();
    return { case: name, passed: true, model_requests: stats.requests.length,
      before: stats.before, after: stats.after };
  } finally {
    resume();
    unsubscribe();
    await bounded(Promise.allSettled([prompting, closing ?? close()]));
  }
}

async function executing({ createSession, root, evidence, config }, timeout) {
  const name = timeout ? "cancel-tool-timeout" : "cancel-executing-tool";
  const cwd = join(root, name);
  mkdirSync(cwd);
  let enter, resume;
  const entered = new Promise((resolve) => { enter = resolve; });
  const released = new Promise((resolve) => { resume = resolve; });
  let toolSignal, observedAbort = false, finished = false;
  const { session, close } = await createSession({ cwd, index: 0,
    environment: { AW_TEST_MODE: "allow", AW_TEST_TENANT: name },
    execute: async (_id, _args, signal) => {
      toolSignal = signal;
      signal.addEventListener("abort", () => { observedAbort = true; }, { once: true });
      enter();
      await released;
      finished = true;
      return { content: [{ type: "text", text: "cancelled tool result" }] };
    },
  });
  const prompting = session.prompt("cancel an executing tool").then(
    () => ({ rejected: false }), () => ({ rejected: true }));
  let closing;
  const status = () => JSON.parse(execFileSync(config.executable,
    ["status", "--config", join(root, "aw.json")], { encoding: "utf8", timeout: 3000 })).result;
  try {
    await bounded(entered);
    let settled = false;
    closing = close().then(() => { settled = true; }, (error) => { settled = true; throw error; });
    // Observe rejection while the native tool deliberately holds teardown open.
    closing.catch(() => {});
    assert(toolSignal.aborted && observedAbort);
    await Promise.resolve();
    assert(!settled && !finished, "close returned before the native tool stopped");
    assert.equal(status().bindings, 2, "scope released while SDK teardown was pending");
    if (timeout) {
      await assert.rejects(bounded(closing, 16000), /SDK teardown deadline exceeded/);
      assert(!finished);
      await assert.rejects(createSession({ cwd, index: 1 }), /requires restart/);
      assert.equal(status().bindings, 1);
    }
    resume();
    const result = await bounded(prompting);
    if (!timeout) await bounded(closing);
    assert(finished && result.rejected);
    const stats = evidence.get(session);
    writeFileSync(join(cwd, "requests.json"), JSON.stringify(stats.requests));
    assert.equal(stats.requests.length, 1);
    assert.equal(stats.before, 1);
    assert.equal(stats.after, 0);
    assert(!existsSync(join(cwd, "effect")));
    return { case: name, passed: true, model_requests: stats.requests.length };
  } finally {
    resume();
    await bounded(Promise.allSettled([prompting, closing ?? close()]));
  }
}

export async function run(options) {
  if (process.env.AW_TEST_SCENARIO === "close-timeout") return [await executing(options, true)];
  const results = [];
  for (const after of [false, true]) {
    const selected = process.env.AW_TEST_PI_CANCEL_CASE;
    if (selected && selected !== (after ? "cancel-after-tool" : "cancel-before-tool")) continue;
    results.push(await cancel(options, after));
  }
  if (!process.env.AW_TEST_PI_CANCEL_CASE
      || process.env.AW_TEST_PI_CANCEL_CASE === "cancel-executing-tool")
    results.push(await executing(options, false));
  for (const after of [false, true]) {
    const selected = process.env.AW_TEST_PI_CANCEL_CASE;
    if (!selected || selected === (after ? "abort-after-tool" : "abort-before-tool"))
      results.push(await cancel(options, after, true));
  }
  assert(results.length, "No Pi cancellation case matched");
  await options.recover();
  return results;
}
