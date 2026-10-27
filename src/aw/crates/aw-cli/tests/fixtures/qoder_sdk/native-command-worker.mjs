/** Exercise wrapper publication and consumer ownership through the official Worker. */
import assert from "node:assert/strict";
import { access, readFile, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const bridge = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const config = JSON.parse(await readFile(process.env.AW_QODER_SDK_CONFIG, "utf8"));
const root = process.env.QODER_COMMAND_ROOT, mode = process.env.QODER_COMMAND_MODE;
const pause = () => new Promise(resolve => setTimeout(resolve, 20));
async function wait(check, label, milliseconds = 90000) {
  const deadline = Date.now() + milliseconds;
  while (!await check()) { assert(Date.now() < deadline, `${label} deadline`); await pause(); }
}
const marker = join(root, "executed"), finish = join(root, "finish");
const slow = ["daemon-timeout-eof", "direct-close"].includes(mode);
const ruleDenied = mode.includes("rule-deny");
const denied = mode === "daemon-deny" || ruleDenied;
const script = join(root, "logical.mjs");
await writeFile(script, `import fs from "node:fs";
fs.writeFileSync(${JSON.stringify(marker)}, "logical command executed");
${slow ? `const deadline = setTimeout(() => process.exit(7), 90000);
const poll = setInterval(() => { if (fs.existsSync(${JSON.stringify(finish)})) {
clearTimeout(deadline); clearInterval(poll); } }, 20);` : ""}
`);
const logical = ruleDenied ? "echo blocked example" :
  `exec ${JSON.stringify(process.execPath)} ${JSON.stringify(script)}`;
const permissions = [], records = [], events = [], messages = [], posts = [];
const nativeProcesses = new Map();
async function recordProcesses() {
  const children = (await readFile(`/proc/${process.pid}/task/${process.pid}/children`, "utf8"))
    .trim().split(/\s+/).filter(Boolean);
  for (const child of children) {
    const command = (await readFile(`/proc/${child}/cmdline`, "utf8")).split("\0").filter(Boolean);
    assert(!command.some(value => value.includes(process.env.QODER_PERSONAL_ACCESS_TOKEN)));
    const fields = (await readFile(`/proc/${child}/stat`, "utf8")).split(") ")[1].split(" ");
    nativeProcesses.set(child, { pid: Number(child), command, cwd: root, ports: [],
      start_ticks: fields[19], pgid: Number(fields[2]), group: false, kind: "native",
      log: join(root, "daemon.log"), deadline_seconds: 140, stop: `kill -TERM ${child}` });
  }
  await writeFile(join(root, "native-processes.json"), JSON.stringify([...nativeProcesses.values()]));
}
let daemon, query, reading, ended = false, readerError, releaseSettlement;
const failures = [];
const settlement = new Promise(resolve => { releaseSettlement = resolve; });
if (ruleDenied) releaseSettlement();
const live = pid => {
  try { process.kill(pid, 0); return true; }
  catch (error) { if (error.code === "ESRCH") return false; throw error; }
};
try {
  if (!mode.startsWith("direct")) daemon = await bridge.startDaemon({
    auth: { type: "accessToken", accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } },
    workerIdleTimeoutMs: 180000, debugFile: join(root, "daemon.log"), stderr: () => {},
  });
  await recordProcesses();
  async function* prompt() {
    yield { type: "user", message: { role: "user", content:
      `Use Bash exactly once with this exact JSON input: ${JSON.stringify({ command: logical,
        ...(slow ? { timeout: 1000 } : {}) })}. After its result reply DONE and stop. ` +
      "Do not retry, do not wait for any background task, and do not use another tool." }, parent_tool_use_id: null };
  }
  query = await bridge.createQuery({ prompt: prompt(), options: {
    ...(daemon ? { daemon } : { auth: { type: "accessToken", accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } } }),
    cwd: root, settingSources: [], persistSession: false, model: "auto", maxTurns: 3,
    tools: ["Bash"], permissionMode: mode.includes("accept-edits") ? "acceptEdits" : "default", stderr: () => {},
    ...(ruleDenied ? { disallowedTools: ["Bash(echo blocked:*)"] } : {}),
    hooks: Object.fromEntries(["PreToolUse", "PostToolUse", "PostToolUseFailure"].map(name => [name,
      [{ hooks: [async input => {
        assert.equal(input.tool_input.command, logical);
        if (name === "PreToolUse") assert.equal(input.permission_mode,
          mode.includes("accept-edits") ? "acceptEdits" : "default");
        else posts.push({ event: name, tool_use_id: input.tool_use_id });
        return {};
      }] }],
    ])),
    canUseTool: async (name, input) => {
      assert.equal(name, "Bash"); assert.equal(input.command, logical);
      assert(!input.command.includes("wrapper.mjs"));
      permissions.push(structuredClone(input));
      return mode === "daemon-deny" ? { behavior: "deny", message: "Native veto; stop without retry" } :
        { behavior: "allow", updatedInput: input };
    },
    awNativeExecutionFactory: async context => {
      assert.equal(context.input.command, logical);
      assert.equal(context.toolName, "Bash");
      const ticket = randomUUID().replaceAll("-", "");
      const record = { ticket, context, settled: false, released: false, entered: false };
      records.push(record);
      await writeFile(join(root, ticket + ".json"), JSON.stringify({ command: logical }), { mode: 0o600 });
      return { version: bridge.NATIVE_COMMAND_WRAPPER_VERSION,
        commandWrapper: { executable: join(root, "wrapper.mjs"), ticket },
        settle: async ({ signal }) => {
          record.entered = true; events.push("settle-entered");
          const cancel = async () => {
            await wait(() => existsSync(join(root, ticket + ".started")), "consumer start", 5000);
            const ids = JSON.parse(await readFile(join(root, ticket + ".started"), "utf8"));
            try {
              const fields = (await readFile(`/proc/${ids.child}/stat`, "utf8")).split(") ")[1].split(" ");
              if (fields[19] !== ids.processes.find(record => record.pid === ids.child).start_ticks) return;
              assert.equal(Number(fields[2]), ids.child);
              process.kill(-ids.child, "SIGTERM");
            } catch (error) { if (!["ESRCH", "ENOENT"].includes(error.code)) throw error; }
          };
          let cancelling;
          const abort = () => { cancelling = cancel(); cancelling.catch(() => {}); };
          signal.addEventListener("abort", abort, { once: true });
          if (signal.aborted) abort();
          try {
            await wait(() => existsSync(join(root, ticket + ".joined")), "consumer join");
            if (cancelling) await cancelling;
            const ids = JSON.parse(await readFile(join(root, ticket + ".started"), "utf8"));
            await wait(() => !live(ids.child) && !live(ids.wrapper), "consumer processes exit", 5000);
            await settlement;
            record.settled = true; events.push("consumer-ACK");
          } finally { signal.removeEventListener("abort", abort); }
        },
        release: async () => {
          if (existsSync(join(root, ticket + ".started"))) assert.equal(record.settled, true);
          record.released = true; events.push("released");
          await writeFile(join(root, ticket + ".revoked"), "revoked");
        },
      };
    },
  } });
  await recordProcesses();
  reading = (async () => { for await (const message of query) messages.push(message); ended = true; events.push("EOF"); })();
  reading.catch(error => { readerError = error; });
  const checked = predicate => () => {
    if (readerError) throw readerError;
    if (ended && !denied && !records.length) throw new Error("Worker ended without a command consumer");
    return predicate();
  };
  if (denied) {
    await wait(checked(() => ended), "native veto EOF");
    if (ruleDenied) {
      assert.equal(permissions.length, 0, "native rule denial must precede the host approver");
      assert(messages.some(message => message.permission_denials?.some(item =>
        item.tool_name === "Bash" && item.tool_input?.command === logical)),
      "Worker omitted the logical command's native permission denial");
    } else assert(permissions.length > 0);
    assert(records.length > 0);
    assert(!existsSync(marker)); assert(records.every(record => record.released && !record.entered));
  } else {
    await wait(checked(() => records.some(record => record.entered)), "settle entry");
    assert.equal(records.length, 1); assert(permissions.length > 0); assert(existsSync(marker));
    assert.equal(records[0].released, false); assert.equal(ended, false);
    if (slow) {
      assert(permissions.some(input => input.timeout === 1000 && input.run_in_background !== true));
      await wait(checked(() => messages.some(message => message.subtype === "background_tasks_changed" && message.tasks.length)),
        "automatic background announcement");
      assert.equal(ended, false); assert.equal(records[0].released, false);
    }
    await wait(checked(() => messages.some(message => message.type === "result")), "finite prompt result");
    assert.equal(ended, false); assert.equal(records[0].released, false);
    if (slow) {
      const ids = JSON.parse(await readFile(join(root, records[0].ticket + ".started"), "utf8"));
      assert(live(ids.wrapper) && live(ids.child), "finite prompt EOF stopped a live consumer");
    }
    if (mode === "direct-close") {
      let closed = false;
      const closing = query.close().then(() => { closed = true; events.push("closed"); });
      await wait(() => existsSync(join(root, records[0].ticket + ".joined")), "cancelled child join", 10000);
      assert.equal(closed, false); assert.equal(records[0].released, false);
      releaseSettlement(); await closing;
      assert.equal(records[0].settled, true); assert.equal(records[0].released, true);
      assert(events.indexOf("consumer-ACK") < events.indexOf("closed"));
    } else {
      if (slow) await writeFile(finish, "finish");
      await wait(() => existsSync(join(root, records[0].ticket + ".joined")), "natural child join", 10000);
      assert.equal(ended, false); assert.equal(records[0].released, false);
      releaseSettlement(); await wait(checked(() => ended), "joined EOF", 30000);
      assert.equal(records[0].settled, true); assert.equal(records[0].released, true);
      assert(events.indexOf("consumer-ACK") < events.indexOf("EOF"));
    }
  }
  await reading; await query.close();
  assert(records.every(record => record.released));
  if (!denied) assert(posts.length > 0, "real Worker omitted the original Post callback");
  await writeFile(join(root, "proof.json"), JSON.stringify({ mode, permission_calls: permissions.length,
    acquisitions: records.length, posts, events, automatic_background: messages.some(message =>
      message.subtype === "background_tasks_changed" && message.tasks.length),
    logical_permission_only: true, native_rule_rejected: ruleDenied,
    consumer_join_before_revoke: true, cleanup: true }));
} catch (error) {
  failures.push(error);
  console.error(JSON.stringify({ mode, ended, permissions: permissions.length, posts, events,
    records: records.map(({ entered, settled, released }) => ({ entered, settled, released })),
    messages: messages.map(message => ({ type: message.type, subtype: message.subtype,
      is_error: message.is_error, tools: message.message?.content?.filter(block => block.type === "tool_use")
        .map(block => ({ name: block.name, logical: block.input?.command === logical })) })),
  }));
}
finally {
  const attempt = async action => {
    let timer;
    try { await Promise.race([Promise.resolve().then(action), new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("worker cleanup deadline")), 10000);
    })]); } catch (error) { failures.push(error); }
    finally { clearTimeout(timer); }
  };
  releaseSettlement();
  await attempt(() => writeFile(finish, "finish"));
  await attempt(() => query?.close());
  await attempt(() => reading);
  await attempt(() => daemon?.stop());
  for (const pid of nativeProcesses.keys())
    await attempt(() => assert.rejects(access(`/proc/${pid}`), { code: "ENOENT" }));
  for (const record of records) await attempt(async () => {
    if (!existsSync(join(root, record.ticket + ".started"))) return;
    const ids = JSON.parse(await readFile(join(root, record.ticket + ".started"), "utf8"));
    for (const pid of [ids.child, ids.wrapper])
      await attempt(() => assert.rejects(access(`/proc/${pid}`), { code: "ENOENT" }));
  });
}
if (failures.length) throw new AggregateError(failures, "hosted wrapper acceptance or cleanup failed");
