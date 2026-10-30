/** Run an unmodified SDK/Worker through the installed AW bridge. */
import assert from "node:assert/strict";
import { access, readFile, writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const sdk = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const entry = JSON.parse(await readFile("case.json", "utf8"));
const daemon = await sdk.startDaemon({ auth: { type: "accessToken",
  accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } } });
try {
  const messages = [];
  const query = await sdk.createQuery({ prompt: entry.prompt, options: {
    daemon, cwd: process.cwd(), model: "auto", tools: [], maxTurns: 1,
    persistSession: false, settingSources: ["user", "project", "local"],
    ...(entry.native_deny ? { hooks: { UserPromptSubmit: [{ hooks: [async () =>
      ({ decision: "block", reason: "Native fixture veto" })] }] } } : {}),
  } });
  let timer, cancelled = false;
  if (entry.cancel) timer = setInterval(async () => {
    try {
      await access("cancel.request");
      clearInterval(timer);
      cancelled = true;
      await query.close();
    } catch {}
  }, 25);
  try { for await (const message of query) messages.push(message); }
  catch (error) { if (!entry.cancel) throw error; }
  finally { clearInterval(timer); await query.close(); }
  const result = messages.filter(message => message.type === "result").at(-1);
  if (entry.cancel) {
    assert(cancelled, "Query completed before cancellation");
    assert(!messages.some(message => message.type === "assistant"));
    if (result) {
      assert.equal(result.num_turns, 0);
      assert.equal(result.duration_api_ms, 0);
    }
  } else if (entry.blocked) {
    assert(result, "native Query did not produce a terminal result");
    assert.equal(result.num_turns, 0);
    assert.equal(result.duration_api_ms, 0);
    assert(!messages.some(message => message.type === "assistant"));
  } else {
    assert(result, "native Query did not produce a terminal result");
    assert(result.num_turns > 0, "clean input never reached the model");
    assert.equal(result.is_error, false);
    assert(messages.some(message => message.type === "assistant"));
  }
  await writeFile("messages.json", JSON.stringify(messages, null, 2));
  await writeFile("result.json", JSON.stringify({ case: entry.case, status: "passed",
    blocked: entry.blocked, cancelled, num_turns: result?.num_turns ?? 0,
    duration_api_ms: result?.duration_api_ms ?? 0 }));
} finally { await daemon.stop(); }
