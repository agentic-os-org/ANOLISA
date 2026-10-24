/** Close during a paused native tool turn must fence the SDK's post-abort model attempt. */
import assert from "node:assert/strict";
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

async function bounded(operation) {
  let timer;
  try {
    return await Promise.race([operation, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("Pi cancellation test exceeded its deadline")), 8000);
    })]);
  } finally { clearTimeout(timer); }
}

async function cancel({ createSession, root, evidence }, after) {
  const name = after ? "cancel-after-tool" : "cancel-before-tool";
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
    closing = close();
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

export async function run(options) {
  const results = [];
  for (const after of [false, true]) {
    const selected = process.env.AW_TEST_PI_CANCEL_CASE;
    if (selected && selected !== (after ? "cancel-after-tool" : "cancel-before-tool")) continue;
    results.push(await cancel(options, after));
  }
  await options.recover();
  return results;
}
