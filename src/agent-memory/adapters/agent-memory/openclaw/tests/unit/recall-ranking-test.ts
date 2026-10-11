import { describe, it } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { MAX_RESULTS, RecallRankFusion } from "../../src/keyword-extract.js";
import { McpStdioClient } from "../../src/mcp-client.js";

function hit(path: string, snippet = path, score = 1) {
  return { path, snippet, score };
}

describe("auto-recall reciprocal-rank fusion", () => {
  it("ranks cross-query consensus above incomparable raw scores", () => {
    const ranking = new RecallRankFusion();
    const first = hit("first", "first", 1000);
    const shared = hit("shared", "shared", 0.001);
    const last = hit("last", "last", 5000);
    ranking.addBatch([first, shared]);
    ranking.addBatch([shared, last]);
    assert.deepEqual(ranking.results(), [shared, first, last]);
  });

  it("keeps the payload from the strongest individual rank", () => {
    const ranking = new RecallRankFusion();
    const lower = hit("same", "lower rank", 1000);
    const higher = hit("same", "higher rank", 0.001);
    ranking.addBatch([hit("other"), lower]);
    ranking.addBatch([higher]);
    assert.equal(ranking.results()[0], higher);
  });

  it("keeps first payloads and insertion order when contributions tie", () => {
    const ranking = new RecallRankFusion();
    const firstA = hit("a", "first a");
    const firstB = hit("b", "first b");
    const bestB = hit("b", "best b");
    ranking.addBatch([firstA, firstB]);
    ranking.addBatch([bestB, hit("a", "later a")]);
    assert.deepEqual(ranking.results(), [firstA, bestB]);
    ranking.addBatch([hit("a", "equal best a")]);
    assert.equal(ranking.results()[0], firstA);
  });

  it("caps injected memories at the existing result limit", () => {
    const ranking = new RecallRankFusion();
    const batch = Array.from({ length: MAX_RESULTS + 2 }, (_, i) => hit(`path-${i}`));
    ranking.addBatch(batch);
    assert.deepEqual(ranking.results(), batch.slice(0, MAX_RESULTS));
  });

  it("uses complete JSON identity for hits without a path", () => {
    const ranking = new RecallRankFusion();
    const first = { snippet: "same", score: 1 };
    const distinct = { snippet: "other", score: 1 };
    ranking.addBatch([first, distinct]);
    ranking.addBatch([{ snippet: "same", score: 1 }]);
    assert.deepEqual(ranking.results(), [first, distinct]);
    assert.equal(ranking.results()[0], first);
  });

  it("does not mutate batches or the original hit objects", () => {
    const ranking = new RecallRankFusion();
    const first = Object.freeze(hit("frozen"));
    const batch = Object.freeze([first]);
    ranking.addBatch(batch);
    assert.equal(ranking.results()[0], first);
    assert.deepEqual(batch, [{ path: "frozen", snippet: "frozen", score: 1 }]);
  });

  it("returns fresh result arrays without consuming accumulated rankings", () => {
    const ranking = new RecallRankFusion();
    const a = hit("a");
    const b = hit("b");
    ranking.addBatch([a, b]);
    ranking.results().reverse();
    assert.deepEqual(ranking.results(), [a, b]);
    ranking.addBatch([b]);
    assert.deepEqual(ranking.results(), [b, a]);
  });

  it("returns no results for empty batches", () => {
    const ranking = new RecallRankFusion();
    ranking.addBatch([]);
    assert.deepEqual(ranking.results(), []);
  });
});

it("the registered prompt hook fuses successful queries across a failed candidate", async (t) => {
  const plugin = (await import("../../src/index.js")).default;
  const binDir = fs.mkdtempSync(path.join(os.tmpdir(), "memory-recall-ranking-"));
  const binaryPath = path.join(binDir, "agent-memory");
  fs.writeFileSync(binaryPath, "#!/bin/sh\nexit 0\n");
  fs.chmodSync(binaryPath, 0o755);
  const realCall = McpStdioClient.prototype.callToolByName;
  const calls: { name: string; args: Record<string, unknown> }[] = [];
  const warnings: string[] = [];
  const hooks = new Map<string, (...args: never[]) => unknown>();
  t.after(() => {
    McpStdioClient.prototype.callToolByName = realCall;
    fs.rmSync(binDir, { recursive: true, force: true });
  });
  McpStdioClient.prototype.callToolByName = async function (name, args) {
    calls.push({ name, args });
    if (calls.length === 1) return JSON.stringify([hit("a", "first memory", 1000), hit("b", "lower payload")]);
    if (calls.length === 2) throw new Error("candidate unavailable");
    return JSON.stringify([hit("b", "best payload", 0.001), hit("c", "last memory", 5000)]);
  };
  plugin.register({
    pluginConfig: { binaryPath, userId: "1000", sessionId: "ses_recall", profile: "advanced" },
    resolvePath: (p: string) => p,
    logger: { info: () => {}, warn: (message: string) => warnings.push(message) },
    on: (event: string, callback: (...args: never[]) => unknown) => hooks.set(event, callback),
    registerTool: () => {},
    registerMemoryCapability: () => {},
    registerMemoryCorpusSupplement: () => {},
  } as never);

  const hook = hooks.get("before_prompt_build")! as unknown as (
    event: { prompt: string }, context: Record<string, unknown>,
  ) => Promise<{ prependContext: string } | undefined>;
  const prompt = "What is the secret codeword for our session today?";
  const result = await hook({ prompt }, {});
  assert.deepEqual(calls.map((call) => call.args.query), ["secret codeword session today", "secret codeword session", prompt]);
  assert.ok(calls.every((call) => call.name === "memory_search" && call.args.mode === "bm25" && call.args.top_k === 5));
  assert.ok(warnings.some((message) => message.includes("candidate unavailable")));
  assert.ok(result);
  assert.match(result.prependContext, /1\. best payload \[path: b,/);
  assert.match(result.prependContext, /2\. first memory \[path: a,/);
  assert.match(result.prependContext, /3\. last memory \[path: c,/);
  assert.doesNotMatch(result.prependContext, /lower payload/);
  await hooks.get("gateway_stop")!();
});
