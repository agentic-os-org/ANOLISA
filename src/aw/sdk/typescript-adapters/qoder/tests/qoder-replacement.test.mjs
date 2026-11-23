import test from "node:test";
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import { Admission, Instance } from "../../../typescript/src/client.mjs";
import { qoderCapabilities, qoderInstallRoutes, qoderOptions } from "../src/index.mjs";
import { before, fixture, rawResult, replacementOutput, reviewedResult } from "./qoder-bridge-fixture.mjs";

const observeResults = async (root) =>
  (await readFile(root + "/post.log", "utf8")).trim().split("\n").map(JSON.parse);

const consume = async (query) => {
  const types = [];
  for await (const message of query) types.push(message.type);
  return types;
};

test("bridge returns the final replacement and preserves the native Post group", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  await f.configureReplacement();
  const bridge = await f.bridge();
  await f.attach();
  const seen = [];
  const group = {
    matcher: "Bash",
    timeout: 4,
    hooks: [async (input) => { seen.push(input.tool_response); return {}; }],
  };
  const options = { hooks: { PostToolUse: [group] }, model: "existing-model" };
  assert.deepEqual(await consume(bridge.query({ prompt: "sensitive", options })), ["model", "result"]);
  assert.deepEqual(f.postOutputs, [{}, replacementOutput]);
  assert.deepEqual(seen, [rawResult]);
  assert.equal(options.hooks.PostToolUse.length, 1);
  assert.deepEqual(f.options[0].hooks.PostToolUse[0], group);
  assert.equal(f.options[0].model, options.model);
  const records = await observeResults(f.root);
  assert.deepEqual(records.map(({ operation }) => operation), ["record", "scan"]);
  const { result_projection, ...replacementTool } = records[1].tool;
  assert.deepEqual(records[0].tool, replacementTool);
  assert.equal(result_projection.text, rawResult);
});

test("a preinstalled native wrapper uses the acknowledged Admission replacement routes", async (t) => {
  const f = await fixture();
  await f.configureReplacement();
  const cwd = f.root + "/work";
  const instance = new Instance({
    endpoint: f.root + "/state/aw.sock",
    target: "app",
    availability: "required",
    cwd,
    capabilities: qoderCapabilities(false),
    install: (routes) => qoderInstallRoutes(routes, false),
  });
  const admission = new Admission(instance, { events: ["tool.after"], cwd });
  t.after(async () => {
    try {
      await admission.close();
    } finally {
      try {
        await instance.close();
      } finally {
        await f.close();
      }
    }
  });
  const integration = qoderOptions(admission);
  const run = integration.options.hooks.PostToolUse[0].hooks[0];
  const payload = {
    ...before,
    hook_event_name: "PostToolUse",
    cwd,
    tool_response: { stdout: rawResult, metadata: "retained" },
  };
  instance.listen();
  await f.attach();
  assert.deepEqual(await run(payload, "call"), {
    hookSpecificOutput: {
      hookEventName: "PostToolUse",
      updatedToolOutput: JSON.stringify({ metadata: "retained", stdout: reviewedResult }),
    },
  });
  assert.equal(integration.failure, undefined);
  assert.equal(admission.routes.hooks.find((h) => h.step === "replace").replace_result, true);
  const records = await observeResults(f.root);
  assert.deepEqual(records.map(({ operation }) => operation), ["record", "scan"]);
});

test("clean failures are observed and sensitive failures have no replaceable slot", async (t) => {
  const f = await fixture();
  t.after(() => f.close());
  await f.configureReplacement();
  const bridge = await f.bridge();
  await f.attach();
  assert.deepEqual(await consume(bridge.query({ prompt: "failure" })), ["model", "result"]);
  assert.deepEqual(f.postOutputs, [{}]);
  await assert.rejects(consume(bridge.query({ prompt: "sensitive-failure" })), /sdk_step_failed/);
  assert.deepEqual(f.postOutputs, [{}, { continue: false, stopReason: "AW result check failed" }]);
  const records = await observeResults(f.root);
  assert.deepEqual(records.map(({ operation }) => operation), ["record", "scan", "record", "scan"]);
  for (const { tool } of records) {
    assert.equal(tool.result.is_error, true);
    assert.equal(tool.result_projection, undefined);
  }
});

test("reachable Post Provider failure surfaces through the public Query in fallback mode", async (t) => {
  const f = await fixture(false);
  t.after(() => f.close());
  await f.configureReplacement();
  const bridge = await f.bridge();
  await f.attach();
  await writeFile(f.root + "/fail", "");
  await assert.rejects(consume(bridge.query({ prompt: "sensitive" })), /sdk_step_failed/);
  assert.deepEqual(f.calls, ["model", "tool", "interrupt"]);
  assert.deepEqual(f.postOutputs, [{ continue: false, stopReason: "AW result check failed" }]);
  assert.equal(bridge.state, "ready");
});

test("loss at Post leaves active and new fallback Queries with only their native result", async (t) => {
  const f = await fixture(false);
  t.after(() => f.close());
  await f.configureReplacement();
  const bridge = await f.bridge();
  await f.attach();
  const options = {
    hooks: { PostToolUse: [{ hooks: [async () => { await f.stopService(); return {}; }] }] },
  };
  for (let i = 0; i < 2; i++)
    assert.deepEqual(await consume(bridge.query({ prompt: "sensitive", options })), ["model", "result"]);
  assert.deepEqual(f.postOutputs, [{}, {}, {}, {}]);
  await assert.rejects(observeResults(f.root), { code: "ENOENT" });
  assert.equal(bridge.state, "unavailable");
});
