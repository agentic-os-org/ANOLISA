import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { after, test } from "node:test";
import { pathToFileURL } from "node:url";

// Exercise the adapter and Core together: mock transport tests cannot prove
// that a published recovery instruction resolves to the exact original.
const sandbox = mkdtempSync(join(tmpdir(), "tokenless-adapter-core-"));
mkdirSync(join(sandbox, "bin"));
symlinkSync(resolve("target/debug/tokenless"), join(sandbox, "bin/tokenless"));
process.env.PATH = `${join(sandbox, "bin")}:${process.env.PATH}`;
process.env.TOKENLESS_DATA_DIR = join(sandbox, "state");
process.env.TOKENLESS_COMPRESSION_ENABLED = "1";
process.env.TOKENLESS_HTML_EXTRACTION_ENABLED = "1";
delete process.env.TOKENLESS_DIFF_COMPRESSION_ENABLED;
process.env.TOKENLESS_STATS_ENABLED = "0";
process.env.TOKENLESS_SLS_ENABLED = "0";
delete process.env.TOKENLESS_STASH_DB;
delete process.env.TOKENLESS_STATS_DB;
process.env.RTK_DB_PATH = join(sandbox, "rtk.db");
after(() => rmSync(sandbox, { recursive: true, force: true }));

const { default: plugin } = await import("../adapters/tokenless/openclaw/dist/index.js");
const hooks = new Map();
let factory;
let middleware;
plugin.register({
  pluginConfig: { rtk_enabled: false, tool_ready_enabled: false },
  registerTool(value) { factory = value; },
  registerAgentToolResultMiddleware(value) { middleware = value; },
  on(name, callback) { hooks.set(name, callback); },
});

// Optional production-host contract replay. Point OPENCLAW_TEST_DIST at an
// installed OpenClaw dist directory; ordinary CI still runs the Core cases below.
test("OpenClaw registry enforces the manifest and dispatches compaction invalidation", {
  skip: !process.env.OPENCLAW_TEST_DIST,
}, async () => {
  const dist = process.env.OPENCLAW_TEST_DIST;
  process.env.OPENCLAW_STATE_DIR = join(sandbox, "openclaw-state");
  const files = readdirSync(dist);
  const loader = await import(pathToFileURL(join(dist, files.find(
    (name) => name.startsWith("loader-") && name.endsWith(".js")
      && readFileSync(join(dist, name), "utf8").includes("function loadOpenClawPlugins("),
  ))).href);
  const hookModule = await import(pathToFileURL(join(dist, files.find(
    (name) => name.startsWith("hook-runner-global-") && name.endsWith(".js"),
  ))).href);
  const loadPlugins = Object.values(loader).find((value) => value.name === "loadOpenClawPlugins");
  const initializeHooks = Object.values(hookModule).find((value) => value.name === "initializeGlobalHookRunner");
  const getHooks = Object.values(hookModule).find((value) => value.name === "getGlobalHookRunner");
  const manifest = JSON.parse(readFileSync("adapters/tokenless/openclaw/openclaw.plugin.json", "utf8"));
  const logger = { info() {}, warn() {}, error() {}, debug() {} };
  let registry;
  for (const declared of [false, true]) {
    const pluginDir = join(sandbox, declared ? "declared" : "undeclared");
    mkdirSync(pluginDir);
    cpSync("adapters/tokenless/openclaw/dist", join(pluginDir, "dist"), { recursive: true });
    cpSync("adapters/tokenless/openclaw/package.json", join(pluginDir, "package.json"));
    const stagedManifest = structuredClone(manifest);
    if (!declared) delete stagedManifest.contracts.tools;
    writeFileSync(join(pluginDir, "openclaw.plugin.json"), JSON.stringify(stagedManifest));
    registry = loadPlugins({
      config: { plugins: {
        allow: ["tokenless"], load: { paths: [pluginDir] }, slots: { memory: "none" },
        entries: { tokenless: {
          enabled: true, config: { rtk_enabled: false, tool_ready_enabled: false },
          hooks: { allowConversationAccess: true },
        } },
      } },
      onlyPluginIds: ["tokenless"], logger, cache: false, activate: false,
    });
    if (!declared) {
      assert.equal(registry.tools.length, 0);
      assert.ok(registry.diagnostics.some((entry) => entry.message.includes("contracts.tools")));
    } else {
      assert.deepEqual(registry.diagnostics.filter((entry) => entry.level === "error"), []);
      assert.deepEqual(registry.tools[0].names, ["tokenless_retrieve"]);
    }
  }
  initializeHooks(registry);
  const runner = getHooks();
  const context = { sessionId: "host-compaction", sessionKey: "agent:host-compaction", runtime: "openclaw" };
  const retrieve = registry.tools[0].factory(context);
  const middleware = registry.agentToolResultMiddlewares[0].handler;
  const original = "<!DOCTYPE html><html><head><title>Report</title></head><body>"
    + `<nav>${"<a href='/menu'>Navigation link</a>".repeat(1000)}</nav>`
    + `<main><h1>Results</h1><p>${"Measured results remain visible. ".repeat(30)}</p></main>`
    + "</body></html>";
  const event = {
    toolName: "web_fetch", toolCallId: "host-fetch", args: {},
    result: { content: [{ type: "text", text: original }] },
  };
  const compressed = await middleware(event, context);
  assert.ok(compressed, "loaded plugin should compress the HTML through host middleware");
  const hash = compressed.result.content[0].text.match(/hash_or_marker=([a-f0-9]{24})/)[1];
  assert.equal((await retrieve.execute("before-compaction", { hash_or_marker: hash })).content[0].text, original);
  // The automatic-compaction callback supplies counts and sessionKey only.
  // There is no intervening llm_input before the following tool call.
  const compaction = runner.runAfterCompaction({ messageCount: 1, compactedCount: 1 }, { sessionKey: context.sessionKey });
  await assert.rejects(retrieve.execute("after-compaction", { hash_or_marker: hash }));
  await compaction;
  await middleware({ ...event, toolCallId: "new-result" }, context);
  assert.equal((await retrieve.execute("new-marker", { hash_or_marker: hash })).content[0].text, original);
});

test("real Core restores HTML exactly with conversation and history authorization", async () => {
  const a = { sessionId: "conversation-a", sessionKey: "agent:a" };
  const b = { sessionId: "conversation-b", sessionKey: "agent:b" };
  const retrieveA = factory(a);
  const retrieveB = factory(b);
  assert.equal(factory({ sessionKey: a.sessionKey }), null);
  const original = '<!DOCTYPE html><html><head><title>Report</title></head><body>'
    + `<nav>${"<a href='/menu'>Navigation link</a>".repeat(35000)}</nav>`
    + `<main><h1>Results</h1><p>${"Measured results remain visible. ".repeat(30)}</p></main>`
    + "</body></html>";
  assert.ok(Buffer.byteLength(original) > 1024 * 1024);
  const event = {
    toolName: "web_fetch", toolCallId: "fetch-page", args: {},
    result: { content: [{ type: "text", text: original }], details: { status: "success" } },
  };
  const compressed = middleware(event, a);
  assert.ok(compressed, "HTML should become a retrievable view");
  const view = compressed.result.content[0].text;
  assert.ok(view.length < original.length);
  const hash = view.match(/hash_or_marker=([a-f0-9]{24})/)?.[1];
  assert.ok(hash, "Core should publish the static-tool recovery instruction");
  assert.deepEqual(compressed.result.details, event.result.details);
  await assert.rejects(retrieveB.execute("read-b", { hash_or_marker: hash }));
  const recovered = await retrieveA.execute("read-a", { hash_or_marker: hash });
  assert.equal(recovered.content[0].text, original);
  assert.equal(middleware({ ...event, toolName: "tokenless_retrieve", result: recovered }, a), undefined);

  hooks.get("after_compaction")({ messageCount: 1, compactedCount: 1 }, { sessionKey: a.sessionKey });
  await assert.rejects(retrieveA.execute("after-compaction", { hash_or_marker: hash }));
  for (const [index, reference] of [
    `<<tokenless:${hash}>>`,
    `If needed, run in shell: tokenless retrieve ${hash}`,
    view,
  ].entries()) {
    const resumed = { sessionId: `resumed-${index}` };
    hooks.get("llm_input")({ historyMessages: [{ role: "tool", content: reference }], prompt: "Continue" }, resumed);
    assert.equal((await factory(resumed).execute("from-history", { hash_or_marker: hash })).content[0].text, original);
  }
  const reset = factory({ sessionId: "conversation-new", sessionKey: a.sessionKey });
  await assert.rejects(reset.execute("after-reset", { hash_or_marker: hash }));
  hooks.get("session_end")(a);
  await assert.rejects(retrieveA.execute("after-end", { hash_or_marker: hash }));
});

test("hosts without a conversation UUID keep HTML unchanged", () => {
  const context = { sessionKey: "agent:unbound" };
  assert.equal(factory(context), null);
  const text = "<!DOCTYPE html><html><body><nav>" + "menu ".repeat(1000)
    + "</nav><main><p>Visible content.</p></main></body></html>";
  assert.equal(middleware({
    toolName: "web_fetch", toolCallId: "no-uuid", args: {},
    result: { content: [{ type: "text", text }] },
  }, context), undefined);
});

test("OpenClaw compresses Diff by default and restores all original context", async () => {
  const context = { sessionId: "diff-conversation", sessionKey: "agent:diff" };
  const retrieve = factory(context);
  const original = "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n"
    + "@@ -1,12 +1,12 @@\n"
    + (" " + "unchanged context ".repeat(20) + "\n").repeat(10)
    + "-old\n+new\n tail\n";
  const response = middleware({
    toolName: "exec", toolCallId: "diff-call", args: { command: "git diff -U10" },
    result: { content: [{ type: "text", text: original }] },
  }, context);
  assert.ok(response, "Diff compression must not need an environment opt-in");
  const view = response.result.content[0].text;
  assert.ok(view.length < original.length);
  assert.ok(view.includes("-old\n+new\n"));
  const hash = view.match(/hash_or_marker=([a-f0-9]{24})/)?.[1];
  assert.ok(hash);
  assert.equal((await retrieve.execute("restore-diff", { hash_or_marker: hash })).content[0].text, original);
});

test("DSH executes the actual RTK suggestion without modifying audited arguments", async () => {
  const { apply } = await import("../adapters/tokenless/dsh/dist/index.js");
  const handlers = new Map();
  apply({
    shellEnv: { register() {} },
    on(name, callback) { handlers.set(name, callback); },
  });
  const repo = join(sandbox, "repo");
  mkdirSync(repo);
  execFileSync("git", ["init", "--quiet", repo]);
  const exec = {
    name: "bash", callId: "original-call",
    agent: { id: "dsh-session", session: { header: { cwd: repo } } },
    arguments: Object.freeze({ command: "git status" }),
  };
  const allow = { kind: "allow" };
  const preTool = handlers.get("tools/pre-execute");
  const suggestion = await preTool(exec, async () => allow);
  assert.equal(suggestion.kind, "deny");
  assert.equal(exec.arguments.command, "git status");
  assert.ok(suggestion.reason.startsWith("[tokenless:rewrite] Re-execute as: "));
  const command = suggestion.reason.slice("[tokenless:rewrite] Re-execute as: ".length);
  const retry = { ...exec, callId: "retry-call", arguments: { command } };
  assert.equal(await preTool(retry, async () => allow), allow);
  const output = execFileSync("bash", ["-c", command], { cwd: repo, encoding: "utf8" });
  assert.ok(output.length > 0);
  const accepted = { kind: "accept" };
  assert.equal(await handlers.get("tools/post-execute")(
    retry, { content: [{ type: "text", text: output }], value: { exit_code: 0 } },
    async () => accepted,
  ), accepted, "RTK output must bypass repeated compression");
});
