/** Published Pi sessions execute deterministic model streams in one resident host. */
import { EventStream } from "@earendil-works/pi-ai/compat";
import { Type } from "typebox";
import { mkdirSync, writeFileSync, readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import assert from "node:assert/strict";

const root = process.env.AW_TEST_ROOT;
const evidence = new WeakMap();
const settings = {
  role: "assistant", api: "openai-responses", provider: "openai", model: "mock", timestamp: 0,
  usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
};

export async function createSession({ sdk, cwd, index, execute, invalidStream }) {
  const agentDir = join(cwd, "agent");
  mkdirSync(agentDir);
  const settingsManager = sdk.SettingsManager.inMemory({ defaultThinkingLevel: "off" });
  const loader = new sdk.DefaultResourceLoader({ cwd, agentDir, settingsManager,
    noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true, noContextFiles: true });
  await loader.reload();
  writeFileSync(join(agentDir, "auth.json"), JSON.stringify({ openai: { type: "api_key", key: "local-fixture" } }));
  const modelRuntime = await sdk.ModelRuntime.create({ authPath: join(agentDir, "auth.json"),
    modelsPath: null, refreshOnCreate: false, allowModelNetwork: false });
  const model = { id: "mock", name: "mock", provider: "openai", api: "openai-responses",
    baseUrl: "http://127.0.0.1:1", reasoning: false, input: ["text"],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 10000, maxTokens: 1000 };
  const result = await sdk.createAgentSession({ cwd, agentDir, modelRuntime, settingsManager,
    resourceLoader: loader, sessionManager: sdk.SessionManager.inMemory(cwd), model, tools: ["test"],
    customTools: [{ name: "test", label: "test", description: "fixture", parameters: Type.Object({}),
      async execute(...args) {
        if (execute) return await execute(...args);
        writeFileSync(join(cwd, "effect"), "executed");
        return { content: [{ type: "text", text: `private-result-${index}` }], details: { index } };
      } }],
  });
  const stats = { before: 0, after: 0, requests: [] };
  evidence.set(result.session, stats);
  const before = result.session.agent.beforeToolCall;
  result.session.agent.beforeToolCall = async (...args) => { stats.before++; return await before(...args); };
  const after = result.session.agent.afterToolCall;
  result.session.agent.afterToolCall = async (...args) => { stats.after++; return await after(...args); };
  result.session.agent.streamFunction = (_model, context) => {
    stats.requests.push(structuredClone(context.messages));
    const done = stats.requests.length % 2 === 0;
    const stream = new EventStream((e) => e.type === "done" || e.type === "error",
      (e) => e.type === "done" ? e.message : e.error);
    const message = { ...settings, stopReason: done ? "stop" : "toolUse", content: done
      ? [{ type: "text", text: "done" }]
      : [{ type: "toolCall", id: "reused-across-tasks", name: "test", arguments: {} }] };
    stream.push({ type: "done", reason: message.stopReason, message });
    stream.end();
    return stream;
  };
  if (invalidStream) result.session.agent.streamFunction = null;
  return result;
}

export async function runWorker({ createSession }) {
  writeFileSync(join(root, "application-started"), "started");
  const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG));
  assert(existsSync(config.ready_path), "host must be ready before first task");
  const results = [];
  const task = async (index, mode) => {
    const cwd = join(root, String(index));
    mkdirSync(cwd);
    const { session, close } = await createSession({ cwd, index,
      environment: { AW_TEST_MODE: mode, AW_TEST_TENANT: String(index) } });
    let failed = false;
    try {
      try { await session.prompt("fixture"); }
      catch (error) { failed = true; if (mode === "allow") throw error; }
      const stats = evidence.get(session);
      writeFileSync(join(cwd, "requests.json"), JSON.stringify(stats.requests));
      assert.equal(failed, mode.endsWith("error"), mode);
      assert.equal(existsSync(join(cwd, "effect")), ["allow", "after-error"].includes(mode), mode);
      assert.equal(stats.requests.length, mode.endsWith("error") ? 1 : 2, mode);
      assert.equal(stats.before, 1);
      assert.equal(stats.after, ["allow", "after-error"].includes(mode) ? 1 : 0);
      if (mode.endsWith("error")) assert(!JSON.stringify(stats.requests).includes(`private-result-${index}`));
      const events = readFileSync(join(cwd, "events.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
      assert(events.every((item) => item.tenant === String(index)));
      assert.equal(new Set(events.map((item) => item.event.agent.instance_id)).size, 1);
      results.push({ mode, task: index, passed: true, instance: events[0].event.agent.instance_id });
    } finally { await close(); }
    await close();
    await assert.rejects(session.prompt("closed task"), /closed/);
  };
  const scenario = process.env.AW_TEST_SCENARIO ?? "lifecycle";
  if (["startup", "cancel", "close-timeout", "drain"].includes(scenario)) {
    const { run } = await import(scenario === "startup" ? "./pi_startup.mjs"
      : scenario === "drain" ? "./pi_drain.mjs" : "./pi_cancel.mjs");
    const result = await run({ createSession, recover: () => task(1, "allow"), root, config, evidence });
    writeFileSync(join(root, "result.json"), JSON.stringify({
      cases: Array.isArray(result) ? result : [result], passed: true }));
    return;
  }
  for (let index = 0; index < 20; index++) await task(index, "allow");
  await Promise.all([20, 21, 22].map((index) => task(index, "allow")));
  for (const [index, mode] of ["deny", "before-error", "after-error"].entries()) await task(23 + index, mode);
  assert.equal(new Set(results.map((item) => item.instance)).size, 26);
  writeFileSync(join(root, "result.json"), JSON.stringify({ tasks: 26, cases: results, passed: true }));
}
