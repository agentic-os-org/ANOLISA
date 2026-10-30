/** Public SDK acceptance: assert adopted tool results and retain messages as evidence. */
import assert from "node:assert/strict";
import { readFile, writeFile, access } from "node:fs/promises";
import { pathToFileURL } from "node:url";
const sdk = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const cases = JSON.parse(await readFile("cases.json", "utf8"));
const daemon = await sdk.startDaemon({ auth: { type: "accessToken",
  accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } } });
const rows = [];
try {
  for (const entry of cases) {
    const messages = [];
    let error = null, timer;
    try {
      const query = await sdk.createQuery({ prompt: entry.prompt, options: {
        daemon, cwd: process.cwd(), tools: entry.tools ?? ["Bash"], model: "auto",
        maxTurns: 3, persistSession: false, permissionMode: "bypassPermissions",
        settingSources: ["user", "project", "local"],
      } });
      if (entry.cancel) timer = setInterval(async () => {
        try { await access("cancel.request"); await query.close(); } catch {}
      }, 50);
      try { for await (const message of query) messages.push(message); }
      finally { clearInterval(timer); await query.close(); }
    } catch (failure) { error = String(failure); }
    const results = messages.flatMap(m => m.type === "user" ? m.message.content : [])
      .filter(block => block.type === "tool_result");
    await writeFile(`${entry.name}.messages.json`, JSON.stringify({ messages, error }, null, 2));
    if (entry.fault) {
      assert.equal(results.length, 0, "unverified result escaped into Query");
      if (!entry.cancel) assert(error, "required output failure was not reported");
    } else {
      assert.equal(error, null);
      assert(results.length > 0, "native tool did not execute or deny");
      if (entry.name.startsWith("tool-output-pii")) {
        assert(!JSON.stringify(results).includes(entry.secret));
        assert(JSON.stringify(results).includes("content withheld"));
        assert(!JSON.stringify(messages.filter(m => m.type === "assistant")).includes(entry.secret));
      }
      if (entry.name === "tool-error") assert(results.some(result => result.is_error));
    }
    rows.push({ case: entry.name, status: "passed", results: results.length, error });
  }
  await writeFile("results.json", JSON.stringify(rows, null, 2));
} finally { await daemon.stop(); }
