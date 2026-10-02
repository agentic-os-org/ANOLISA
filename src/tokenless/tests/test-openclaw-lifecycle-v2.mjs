import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { after, beforeEach, test } from "node:test";

const testDir = dirname(fileURLToPath(import.meta.url));
const sandbox = mkdtempSync(join(tmpdir(), "tokenless-openclaw-v2-"));
const fakeBinDir = join(sandbox, "bin");
const fakeTokenless = join(fakeBinDir, "tokenless");
const requestLog = join(sandbox, "requests.jsonl");
const behaviorFile = join(sandbox, "behavior");
const originalHome = process.env.HOME;
const originalPath = process.env.PATH;

mkdirSync(fakeBinDir, { recursive: true });
writeFileSync(
  fakeTokenless,
  [
    "#!/usr/bin/env node",
    'const fs = require("node:fs");',
    `const requestLog = ${JSON.stringify(requestLog)};`,
    `const behaviorFile = ${JSON.stringify(behaviorFile)};`,
    'if (process.argv.slice(2).join(" ") !== "compress") process.exit(2);',
    'const request = JSON.parse(fs.readFileSync(0, "utf8"));',
    'fs.appendFileSync(requestLog, JSON.stringify({ argv: process.argv.slice(2), request }) + "\\n");',
    'const behavior = fs.existsSync(behaviorFile) ? fs.readFileSync(behaviorFile, "utf8") : "normal";',
    'if (behavior === "exit") process.exit(1);',
    'if (behavior === "malformed") { process.stdout.write("{bad"); process.exit(0); }',
    'let result;',
    'if (request.operation === "pre_tool") {',
    '  const command = request.input.arguments[request.input.command_field];',
    '  if (command === "no-rewrite") {',
    '    result = { arguments: request.input.arguments, action: "passthrough", output_optimization: "none" };',
    '  } else {',
    '    result = {',
    '      arguments: { ...request.input.arguments, [request.input.command_field]: `/mock/rtk ${command}` },',
    '      action: "replace_arguments",',
    '      output_optimization: "rtk",',
    '    };',
    '  }',
    '} else if (request.operation === "post_tool") {',
    '  const input = request.input;',
    '  if (input.status === "error") {',
    '    result = { output: input.content, disposition: "tool_error", additional_context: "install the missing dependency" };',
    '  } else if (input.output_optimization === "rtk" || input.content_origin === "file_content") {',
    '    result = { output: input.content, disposition: "passthrough" };',
    '  } else if (input.content.includes("lossy")) {',
    '    result = { output: input.content, disposition: "recoverability_unavailable" };',
    '  } else if (input.capabilities.replace_with_text) {',
    '    result = { output: "compressed text", disposition: "applied" };',
    '  } else {',
    '    const parsed = JSON.parse(input.content);',
    '    delete parsed.debug;',
    '    result = { output: JSON.stringify(parsed), disposition: "applied" };',
    '  }',
    '} else {',
    '  process.exit(2);',
    '}',
    'process.stdout.write(JSON.stringify({',
    '  protocol_version: 2,',
    '  operation: request.operation,',
    '  attribution: request.attribution,',
    '  result,',
    '}));',
    "",
  ].join("\n"),
);
chmodSync(fakeTokenless, 0o755);

process.env.HOME = sandbox;
process.env.PATH = `${fakeBinDir}:${originalPath || ""}`;

const pluginPath = resolve(testDir, "../adapters/tokenless/openclaw/dist/index.js");
assert.equal(
  existsSync(pluginPath),
  true,
  "OpenClaw plugin build missing; run `make build-openclaw-plugin` first",
);
const { default: plugin } = await import(pathToFileURL(pluginPath).href);

const handlers = new Map();
plugin.register({
  config: {
    rtk_enabled: false,
    post_tool_enabled: false,
    tool_ready_enabled: true,
    verbose: false,
  },
  pluginConfig: {
    rtk_enabled: true,
    post_tool_enabled: true,
    tool_ready_enabled: false,
    verbose: false,
  },
  on(name, handler) {
    const registered = handlers.get(name) || [];
    registered.push(handler);
    handlers.set(name, registered);
  },
});

const handler = (name) => {
  const registered = handlers.get(name) || [];
  assert.equal(registered.length, 1, `expected one ${name} handler`);
  return registered[0];
};
const beforeToolCall = handler("before_tool_call");
const toolResultPersist = handler("tool_result_persist");
const sessionStart = handler("session_start");
const sessionEnd = handler("session_end");

function requests() {
  if (!existsSync(requestLog)) return [];
  return readFileSync(requestLog, "utf8")
    .trim()
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
}

function rewrite(command, context, extraParams = {}) {
  const { toolCallId, ...hookContext } = context;
  return beforeToolCall(
    { toolName: "exec", toolCallId, params: { command, ...extraParams } },
    { toolName: "exec", ...hookContext },
  );
}

function persist(toolName, toolCallId, message, context = {}, extraEvent = {}) {
  return toolResultPersist(
    { toolName, toolCallId, message, ...extraEvent },
    { toolName, toolCallId, ...context },
  );
}

beforeEach(() => {
  rmSync(requestLog, { force: true });
  rmSync(behaviorFile, { force: true });
});

after(() => {
  if (originalHome === undefined) delete process.env.HOME;
  else process.env.HOME = originalHome;
  if (originalPath === undefined) delete process.env.PATH;
  else process.env.PATH = originalPath;
  rmSync(sandbox, { recursive: true, force: true });
});

test("plugin manifest exposes only lifecycle policy switches", () => {
  const manifest = JSON.parse(
    readFileSync(resolve(testDir, "../adapters/tokenless/openclaw/openclaw.plugin.json"), "utf8"),
  );
  assert.deepEqual(Object.keys(manifest.configSchema.properties).sort(), [
    "post_tool_enabled",
    "rtk_enabled",
    "tool_ready_enabled",
    "verbose",
  ]);
});

test("plugin reads lifecycle switches from OpenClaw pluginConfig", () => {
  const disabledHandlers = new Map();
  plugin.register({
    config: {
      rtk_enabled: true,
      post_tool_enabled: true,
      tool_ready_enabled: true,
    },
    pluginConfig: {
      rtk_enabled: false,
      post_tool_enabled: false,
      tool_ready_enabled: false,
      verbose: false,
    },
    on(name, hookHandler) {
      const registered = disabledHandlers.get(name) || [];
      registered.push(hookHandler);
      disabledHandlers.set(name, registered);
    },
  });

  assert.deepEqual([...disabledHandlers.keys()].sort(), ["session_end", "session_start"]);
});

test("resumed non-exec tools retain UUID attribution when RTK is disabled", () => {
  const postOnlyHandlers = new Map();
  plugin.register({
    config: {
      rtk_enabled: false,
      post_tool_enabled: true,
      tool_ready_enabled: false,
      verbose: false,
    },
    pluginConfig: {
      rtk_enabled: false,
      post_tool_enabled: true,
      tool_ready_enabled: false,
      verbose: false,
    },
    on(name, hookHandler) {
      const registered = postOnlyHandlers.get(name) || [];
      registered.push(hookHandler);
      postOnlyHandlers.set(name, registered);
    },
  });

  const beforeHandlers = postOnlyHandlers.get("before_tool_call") || [];
  const persistHandlers = postOnlyHandlers.get("tool_result_persist") || [];
  assert.equal(beforeHandlers.length, 1);
  assert.equal(persistHandlers.length, 1);

  beforeHandlers[0](
    { toolName: "web_fetch", toolCallId: "call-resumed", params: {} },
    {
      toolName: "web_fetch",
      toolCallId: "call-resumed",
      sessionId: "session-actual",
      sessionKey: "agent:main:resumed",
    },
  );
  persistHandlers[0](
    { toolName: "web_fetch", toolCallId: "call-resumed", message: "payload" },
    {
      toolName: "web_fetch",
      toolCallId: "call-resumed",
      sessionKey: "agent:main:resumed",
    },
  );

  const request = requests()[0].request;
  assert.equal(request.attribution.session_id, "session-actual");
  assert.equal(request.input.output_optimization, "none");
});

test("PreTool sends one Protocol v2 operation and applies all returned arguments", () => {
  const result = rewrite(
    "git status",
    { sessionId: "session-1", toolCallId: "call-1" },
    { cwd: "/workspace", env: { KEEP: "yes" } },
  );

  assert.deepEqual(result, {
    params: {
      command: "/mock/rtk git status",
      cwd: "/workspace",
      env: { KEEP: "yes" },
    },
  });
  const logged = requests();
  assert.equal(logged.length, 1);
  assert.deepEqual(logged[0].argv, ["compress"]);
  assert.equal(logged[0].request.protocol_version, 2);
  assert.equal(logged[0].request.operation, "pre_tool");
  assert.deepEqual(logged[0].request.attribution, {
    agent_id: "openclaw",
    session_id: "session-1",
    tool_use_id: "call-1",
  });
  assert.deepEqual(logged[0].request.input.capabilities, {
    replace_arguments: true,
    block_and_suggest: false,
  });
});

test("PreTool requires a stable tool call ID and fails open", () => {
  assert.equal(rewrite("git status", { sessionId: "session-2" }), undefined);
  assert.equal(requests().length, 0);

  writeFileSync(behaviorFile, "malformed");
  assert.equal(
    rewrite("git status", { sessionId: "session-2", toolCallId: "call-bad" }),
    undefined,
  );
  assert.equal(requests().length, 1);
});

test("RTK output optimization is isolated and consumed once", () => {
  rewrite("first", { sessionId: "session-a", toolCallId: "call-a" });
  rewrite("second", { sessionId: "session-b", toolCallId: "call-b" });

  rmSync(requestLog, { force: true });
  assert.equal(persist("exec", "call-other", "other", { sessionId: "session-a" }).message, "compressed text");
  assert.equal(persist("exec", "call-a", "rtk output", { sessionId: "session-a" }), undefined);
  assert.equal(persist("exec", "call-a", "next output", { sessionId: "session-a" }).message, "compressed text");
  assert.equal(persist("exec", "call-b", "rtk output", { sessionId: "session-b" }), undefined);

  const optimizations = requests().map(({ request }) => request.input.output_optimization);
  assert.deepEqual(optimizations, ["none", "rtk", "none", "rtk"]);
});

test("synthetic results consume matching RTK state without invoking Core", () => {
  rewrite("command", { sessionId: "session-synthetic", toolCallId: "call-synthetic" });
  rmSync(requestLog, { force: true });

  assert.equal(
    persist(
      "exec",
      "call-synthetic",
      "synthetic",
      { sessionId: "session-synthetic" },
      { isSynthetic: true },
    ),
    undefined,
  );
  assert.equal(requests().length, 0);
  assert.equal(
    persist("exec", "call-synthetic", "final", { sessionId: "session-synthetic" }).message,
    "compressed text",
  );
  assert.equal(requests()[0].request.input.output_optimization, "none");
});

test("session mapping carries lifecycle attribution and session end clears state", () => {
  sessionStart({ sessionKey: "agent:main:one", sessionId: "session-mapped" });
  rewrite("mapped", { sessionKey: "agent:main:one", toolCallId: "call-mapped" });
  rmSync(requestLog, { force: true });

  persist("exec", "call-mapped", "mapped output", { sessionKey: "agent:main:one" });
  let request = requests()[0].request;
  assert.equal(request.attribution.session_id, "session-mapped");
  assert.equal(request.input.output_optimization, "rtk");

  rewrite("cleanup", { sessionKey: "agent:main:one", toolCallId: "call-cleanup" });
  sessionEnd({ sessionKey: "agent:main:one" });
  rmSync(requestLog, { force: true });

  persist("exec", "call-cleanup", "after end", { sessionId: "session-mapped" });
  request = requests()[0].request;
  assert.equal(request.input.output_optimization, "none");
});

test("resumed sessions retain PreTool identity when PostTool only has a session key", () => {
  rewrite("resumed", {
    sessionId: "session-existing",
    sessionKey: "agent:main:existing",
    toolCallId: "call-existing",
  });
  rmSync(requestLog, { force: true });

  persist(
    "exec",
    "call-existing",
    "rtk output",
    { sessionKey: "agent:main:existing" },
  );
  const request = requests()[0].request;
  assert.equal(request.attribution.session_id, "session-existing");
  assert.equal(request.input.output_optimization, "rtk");
});

test("PostTool restores structured output and maps content origins", () => {
  const structured = { records: [{ id: 1 }], debug: "drop" };
  assert.deepEqual(
    persist("web_fetch", "call-api", structured, { sessionId: "session-api" }),
    { message: { records: [{ id: 1 }] } },
  );
  assert.equal(requests()[0].request.input.content_origin, "api_response");
  assert.equal(requests()[0].request.input.capabilities.replace_with_text, false);

  rmSync(requestLog, { force: true });
  assert.equal(
    persist("read", "call-read", "authoritative file", { sessionId: "session-read" }),
    undefined,
  );
  assert.equal(requests()[0].request.input.content_origin, "file_content");
  assert.equal(requests()[0].request.input.capabilities.recovery.kind, "none");
});

test("PostTool preserves a text tool-result envelope", () => {
  const message = {
    role: "toolResult",
    toolCallId: "call-envelope",
    toolName: "web_fetch",
    content: [{ type: "text", text: "compress me", mime: "text/plain" }],
    details: { source: "test" },
    timestamp: 42,
  };
  const result = persist(
    "web_fetch",
    "call-envelope",
    message,
    { sessionId: "session-envelope" },
  );

  assert.deepEqual(result, {
    message: {
      ...message,
      content: [{ type: "text", text: "compressed text", mime: "text/plain" }],
    },
  });
});

test("PostTool passes through media and multi-block results without spawning", () => {
  const media = {
    role: "toolResult",
    content: [{ type: "image", data: "base64" }],
  };
  const multiple = {
    role: "toolResult",
    content: [
      { type: "text", text: "one" },
      { type: "text", text: "two" },
    ],
  };

  assert.equal(persist("image", "call-media", media), undefined);
  assert.equal(persist("web_fetch", "call-multiple", multiple), undefined);
  assert.equal(requests().length, 0);
});

test("PostTool appends Core diagnostics without replacing the tool error", () => {
  const message = {
    role: "toolResult",
    toolCallId: "call-error",
    toolName: "exec",
    isError: true,
    content: [{ type: "text", text: "command not found" }],
    details: { exitCode: 127 },
  };
  const result = persist("exec", "call-error", message, { sessionId: "session-error" });

  assert.deepEqual(result, {
    message: {
      ...message,
      content: [
        { type: "text", text: "command not found" },
        { type: "text", text: "install the missing dependency" },
      ],
    },
  });
  assert.equal(requests()[0].request.input.status, "error");
  assert.equal(requests()[0].request.input.content_origin, "command_output");
});

test("PostTool rejects lossy output without authorized recovery and fails open", () => {
  assert.equal(persist("web_fetch", "call-lossy", "lossy payload"), undefined);
  assert.equal(requests()[0].request.input.capabilities.recovery.kind, "none");

  rmSync(requestLog, { force: true });
  writeFileSync(behaviorFile, "exit");
  assert.equal(persist("web_fetch", "call-exit", "payload"), undefined);
  assert.equal(requests().length, 1);
});

test("stale optimization state is pruned on the next rewrite", () => {
  const realNow = Date.now;
  try {
    Date.now = () => 1;
    rewrite("old", { sessionId: "session-ttl", toolCallId: "call-old" });
    Date.now = () => 25 * 60 * 60 * 1000;
    rewrite("new", { sessionId: "session-ttl", toolCallId: "call-new" });
  } finally {
    Date.now = realNow;
  }

  rmSync(requestLog, { force: true });
  persist("exec", "call-old", "old output", { sessionId: "session-ttl" });
  assert.equal(requests()[0].request.input.output_optimization, "none");
});
