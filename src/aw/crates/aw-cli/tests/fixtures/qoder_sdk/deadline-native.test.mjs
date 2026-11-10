/** Pinned SDK/Worker Hook timers with hosted model calls and an intentionally stalled AW close. */
import test from "node:test";
import assert from "node:assert/strict";
import { access, mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { deadlineFixture, retryReason } from "./deadline-support.mjs";

const sdkPath = resolve(process.argv[2]), output = resolve(process.argv[3]), aw = resolve(process.argv[4]);
const sdk = await import(pathToFileURL(sdkPath));
assert(process.env.QODER_PERSONAL_ACCESS_TOKEN, "native Hook deadline acceptance requires a Qoder test credential");
await mkdir(output, { recursive: true });
for (const mode of ["denial", "timeout"])
  test(`official Worker delivers late ${mode} after stalled cleanup`, { timeout: 120000 }, async t => {
    const f = await deadlineFixture(t, aw, mode);
    const observed = [], messages = [], marker = join(f.root, "executed");
    const group = { ...f.group, hooks: [async (...args) => {
      const started = performance.now();
      const result = await f.group.hooks[0](...args);
      await f.verify(args[0], result, performance.now() - started);
      observed.push({ result, elapsed_ms: performance.now() - started });
      return result;
    }] };
    const query = sdk.query({ prompt: `Use Bash exactly once with ${JSON.stringify({ command: `printf fixture > ${marker}` })}. If denied, report the denial and stop without retrying or using another tool.`,
      options: { auth: sdk.accessTokenFromEnv(), cwd: f.root, settingSources: [], persistSession: false,
        tools: ["Bash"], permissionMode: "bypassPermissions", model: "auto", maxTurns: 2,
        env: { ...process.env, QODER_CONFIG_DIR: join(f.root, "profile") },
        hooks: { PreToolUse: [group] }, stderr: () => {} } });
    t.after(() => query.close());
    for await (const message of query) messages.push(message);
    assert.equal(observed.length, 1, "real Worker must receive one concrete AW denial");
    const terminal = messages.find(message => message.type === "result");
    assert(terminal && !terminal.is_error, `native result failed: ${terminal?.subtype}`);
    const reason = mode === "timeout" ? "Tool blocked by AW policy" : retryReason;
    const results = messages.flatMap(message => message.type === "user" ? message.message.content : [])
      .filter(item => item.type === "tool_result");
    const text = results.flatMap(result => typeof result.content === "string" ? [result.content] :
      (result.content ?? []).map(part => part.text ?? ""));
    assert(text.some(value => value.includes(reason)), "actual native tool result lost the AW denial reason");
    await assert.rejects(access(marker), { code: "ENOENT" });
    await query.close();
    await writeFile(join(output, `${mode}.json`), JSON.stringify({ passed: true,
      native_timer: true, side_effect: false, callback_count: observed.length,
      callback_elapsed_ms: observed[0].elapsed_ms, native_timeout_ms: group.timeout * 1000,
      reason_preserved: true, correlated_close: true, cleanup: true }));
  });
