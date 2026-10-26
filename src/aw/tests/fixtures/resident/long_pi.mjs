/** Keep one real Pi session through 1,280 call pairs with both AW phases enabled. */
import assert from "node:assert/strict";
import { readFileSync, writeFileSync, mkdirSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { EventStream } from "@earendil-works/pi-ai/compat";
import { createSession as original } from "./resident-fixture.mjs";

const root = process.env.AW_TEST_ROOT;
const stats = { remaining:0, calls:0, before:0, after:0, model:0 };
const settings = { role:"assistant", api:"openai-responses", provider:"openai", model:"mock", timestamp:0,
  usage:{ input:0, output:0, cacheRead:0, cacheWrite:0, totalTokens:0,
    cost:{ input:0, output:0, cacheRead:0, cacheWrite:0, total:0 } } };

export async function createSession(options) {
  const result = await original(options);
  const agent = result.session.agent;
  const before = agent.beforeToolCall;
  const after = agent.afterToolCall;
  agent.beforeToolCall = async (...args) => { stats.before++; return await before(...args); };
  agent.afterToolCall = async (...args) => { stats.after++; return await after(...args); };
  agent.streamFunction = (_model, context) => {
    stats.model++;
    const results = context.messages.filter(message => message.role === "toolResult");
    if (results.length) assert.equal(results.at(-1).content[0].text, "private-result-0");
    const done = stats.remaining === 0;
    if (!done) { stats.remaining--; stats.calls++; }
    const message = { ...settings, stopReason:done ? "stop" : "toolUse", content:done
      ? [{ type:"text", text:"done" }]
      : [{ type:"toolCall", id:"native-id-reused", name:"test", arguments:{} }] };
    const stream = new EventStream(event => ["done","error"].includes(event.type),
      event => event.type === "done" ? event.message : event.error);
    stream.push({ type:"done", reason:message.stopReason, message });
    stream.end();
    return stream;
  };
  return result;
}

function processStats(pid) {
  const directory = `/proc/${pid}`;
  const status = readFileSync(`${directory}/status`, "utf8");
  return { pid, rss_kib:Number(status.match(/^VmRSS:\s+(\d+)/m)[1]),
    threads:Number(status.match(/^Threads:\s+(\d+)/m)[1]), fds:readdirSync(`${directory}/fd`).length };
}

export async function runWorker({ createSession }) {
  const cwd = join(root,"long");
  mkdirSync(cwd);
  const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG));
  const { session, close } = await createSession({ cwd, index:0,
    environment:{ AW_TEST_MODE:"allow", AW_TEST_TENANT:"long" } });
  const id = session.sessionManager.getSessionId();
  const samples = [];
  const started = performance.now();
  try {
    for (const calls of [768,128,128,128,128]) {
      stats.remaining = calls;
      await session.prompt("Run fixture sequence");
      assert.equal(stats.remaining,0);
      assert.equal(session.sessionManager.getSessionId(),id);
      const status = JSON.parse(execFileSync(config.executable,["status","--config",join(root,"aw.json")],{timeout:5000})).result;
      assert.equal(status.resources.usage.invocations,0);
      assert.equal(status.resources.usage.correlations,0);
      samples.push({ calls:stats.calls, seconds:(performance.now()-started)/1000,
        aw:processStats(status.pid), worker:processStats(process.pid), resources:status.resources,
        history_messages:session.agent.state.messages.length });
    }
    assert.equal(stats.calls,1280);
    assert.equal(stats.before,1280);
    assert.equal(stats.after,1280);
    for (const name of ["aw","worker"]) {
      assert(samples.at(-1)[name].rss_kib - samples[0][name].rss_kib < 65536);
      assert(samples.at(-1)[name].fds <= samples[0][name].fds + 4);
      assert(samples.at(-1)[name].threads <= samples[0][name].threads + 4);
    }
    writeFileSync(join(root,"result.json"),JSON.stringify({ tasks:1, calls:stats.calls,
      executions:samples.length, samples, stats, passed:true }));
  } finally { await close(); }
}
