import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
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
    'if (behavior === "timeout") Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20000);',
    "let result;",
    'if (request.operation === "pre_tool") {',
    "  const command = request.input.arguments[request.input.command_field];",
    '  if (command === "no-rewrite") {',
    '    result = { arguments: request.input.arguments, action: "passthrough", output_optimization: "none" };',
    "  } else {",
    "    result = {",
    "      arguments: { ...request.input.arguments, [request.input.command_field]: `/mock/rtk ${command}` },",
    '      action: "replace_arguments",',
    '      output_optimization: "rtk",',
    "    };",
    "  }",
    '} else if (request.operation === "post_tool") {',
    "  const input = request.input;",
    '  if (behavior === "no-savings") {',
    '    result = { output: input.content, disposition: "no_savings" };',
    '  } else if (input.status === "error") {',
    '    result = { output: input.content, disposition: "tool_error", additional_context: "install the missing dependency" };',
    '  } else if (input.output_optimization === "rtk" || input.content_origin === "file_content") {',
    '    result = { output: input.content, disposition: "passthrough" };',
    '  } else if (input.content.includes("lossy")) {',
    '    result = { output: input.content, disposition: "recoverability_unavailable" };',
    "  } else if (input.capabilities.replace_with_text) {",
    '    result = { output: "compressed text", disposition: "applied" };',
    "  } else {",
    "    const parsed = JSON.parse(input.content);",
    "    delete parsed.debug;",
    '    result = { output: JSON.stringify(parsed), disposition: "applied" };',
    "  }",
    "} else {",
    "  process.exit(2);",
    "}",
    'if (behavior === "invalid-output") result.output = 17;',
    "process.stdout.write(JSON.stringify({",
    '  protocol_version: behavior === "wrong-version" ? 1 : 2,',
    "  operation: request.operation,",
    "  attribution: request.attribution,",
    "  result,",
    "}));",
    "",
  ].join("\n"),
);
chmodSync(fakeTokenless, 0o755);

process.env.HOME = sandbox;
process.env.PATH = `${fakeBinDir}:${originalPath || ""}`;

const pluginPath = resolve(
  testDir,
  "../adapters/tokenless/openclaw/dist/index.js",
);
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
  registerAgentToolResultMiddleware(handler, options) {
    handlers.set("middleware", [handler]);
    assert.deepEqual(options.runtimes, ["openclaw"]);
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
const toolResultMiddleware = handler("middleware");
assert.equal(handlers.has("tool_result_persist"), false);
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

function postTool(toolName, toolCallId, result, context = {}, extraEvent = {}) {
  if (typeof result === "string")
    result = { content: [{ type: "text", text: result }] };
  return toolResultMiddleware(
    { toolName, toolCallId, result, args: {}, ...extraEvent },
    context,
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
    readFileSync(
      resolve(testDir, "../adapters/tokenless/openclaw/openclaw.plugin.json"),
      "utf8",
    ),
  );
  assert.deepEqual(Object.keys(manifest.configSchema.properties).sort(), [
    "post_tool_enabled",
    "rtk_enabled",
    "tool_ready_enabled",
    "verbose",
  ]);
  assert.deepEqual(manifest.contracts.agentToolResultMiddleware, ["openclaw"]);
  const metadata = JSON.parse(
    readFileSync(
      resolve(testDir, "../adapters/tokenless/openclaw/package.json"),
      "utf8",
    ),
  );
  assert.equal(metadata.openclaw.compat.pluginApi, ">=2026.6.6");
});

test("missing or rejected middleware disables PostTool while retaining PreTool", () => {
  for (const registerMiddleware of [
    undefined,
    () => {
      throw new Error("registration rejected");
    },
  ]) {
    const hooks = new Map();
    const warnings = [];
    plugin.register({
      pluginConfig: {
        rtk_enabled: true,
        post_tool_enabled: true,
        tool_ready_enabled: false,
      },
      logger: { warn: (message) => warnings.push(message) },
      registerAgentToolResultMiddleware: registerMiddleware,
      on(name, hook) {
        hooks.set(name, hook);
      },
    });
    assert.equal(hooks.has("before_tool_call"), true);
    assert.equal(hooks.has("tool_result_persist"), false);
    assert.equal(warnings.length, 1);
    assert.match(warnings[0], /PostTool disabled/);
  }
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
    registerAgentToolResultMiddleware(hookHandler) {
      disabledHandlers.set("middleware", [hookHandler]);
    },
    on(name, hookHandler) {
      const registered = disabledHandlers.get(name) || [];
      registered.push(hookHandler);
      disabledHandlers.set(name, registered);
    },
  });

  assert.deepEqual([...disabledHandlers.keys()].sort(), [
    "session_end",
    "session_start",
  ]);
});

test("binary discovery skips searchable directories and follows executable symlinks", async () => {
  const shadow = join(sandbox, "path-shadow");
  const binaries = join(sandbox, "path-symlinks");
  mkdirSync(join(shadow, "tokenless"), { recursive: true });
  mkdirSync(binaries);
  symlinkSync(fakeTokenless, join(binaries, "tokenless"));
  const previousPath = process.env.PATH;
  try {
    process.env.PATH = `${shadow}:${binaries}:${previousPath}`;
    const freshUrl = pathToFileURL(pluginPath);
    freshUrl.searchParams.set("binary-discovery", "directory-shadow");
    const { default: freshPlugin } = await import(freshUrl.href);
    const hooks = new Map();
    freshPlugin.register({
      pluginConfig: {
        rtk_enabled: false,
        post_tool_enabled: true,
        tool_ready_enabled: false,
      },
      registerAgentToolResultMiddleware(hook) {
        hooks.set("middleware", hook);
      },
      on(name, hook) {
        hooks.set(name, hook);
      },
    });
    const message = { content: [{ type: "text", text: "compress me" }] };
    const result = hooks.get("middleware")(
      {
        toolName: "web_fetch",
        toolCallId: "directory-shadow",
        args: {},
        result: message,
      },
      {
        sessionId: "directory-shadow-session",
        toolName: "web_fetch",
        toolCallId: "directory-shadow",
      },
    );
    assert.equal(result?.result.content[0].text, "compressed text");
    assert.equal(requests().length, 1);
    hooks.get("before_tool_call")(
      {
        toolName: "exec",
        toolCallId: "directory-shadow-file",
        params: { command: "cat page.html" },
      },
      {
        sessionId: "directory-shadow-session",
        toolName: "exec",
        toolCallId: "directory-shadow-file",
      },
    );
    hooks.get("middleware")(
      {
        toolName: "exec",
        toolCallId: "directory-shadow-file",
        args: {},
        result: { content: [{ type: "text", text: "<html></html>" }] },
      },
      {
        sessionId: "directory-shadow-session",
        toolName: "exec",
        toolCallId: "directory-shadow-file",
      },
    );
    const fileRead = requests()[1].request.input;
    assert.equal(fileRead.content_origin, "command_output");
    assert.equal(fileRead.command, "cat page.html");
    assert.equal(fileRead.output_optimization, "none");
  } finally {
    process.env.PATH = previousPath;
  }
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
    registerAgentToolResultMiddleware(hookHandler) {
      postOnlyHandlers.set("middleware", [hookHandler]);
    },
    on(name, hookHandler) {
      const registered = postOnlyHandlers.get(name) || [];
      registered.push(hookHandler);
      postOnlyHandlers.set(name, registered);
    },
  });

  const beforeHandlers = postOnlyHandlers.get("before_tool_call") || [];
  const middlewareHandlers = postOnlyHandlers.get("middleware") || [];
  assert.equal(beforeHandlers.length, 1);
  assert.equal(middlewareHandlers.length, 1);

  beforeHandlers[0](
    { toolName: "web_fetch", toolCallId: "call-resumed", params: {} },
    {
      toolName: "web_fetch",
      toolCallId: "call-resumed",
      sessionId: "session-actual",
      sessionKey: "agent:main:resumed",
    },
  );
  middlewareHandlers[0](
    {
      toolName: "web_fetch",
      toolCallId: "call-resumed",
      args: {},
      result: { content: [{ type: "text", text: "payload" }] },
    },
    {
      toolName: "web_fetch",
      toolCallId: "call-resumed",
      sessionKey: "agent:main:resumed",
    },
  );

  const request = requests()[0].request;
  assert.equal(request.attribution.session_id, "session-actual");
  assert.equal(request.input.output_optimization, "none");
  assert.equal(request.input.command, undefined);

  // Core classifies plain file prints as file_read from the shell command line,
  // so the line is recorded before the call even when RTK is off.
  beforeHandlers[0](
    {
      toolName: "exec",
      toolCallId: "call-cat",
      params: { command: "cat page.html" },
    },
    { toolName: "exec", toolCallId: "call-cat", sessionId: "session-actual" },
  );
  middlewareHandlers[0](
    {
      toolName: "exec",
      toolCallId: "call-cat",
      args: {},
      result: { content: [{ type: "text", text: "<html></html>" }] },
    },
    { toolName: "exec", toolCallId: "call-cat", sessionId: "session-actual" },
  );
  const fileRead = requests()[1].request.input;
  assert.equal(fileRead.content_origin, "command_output");
  assert.equal(fileRead.command, "cat page.html");
  assert.equal(fileRead.output_optimization, "none");
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
  assert.equal(
    postTool("exec", "call-other", "other", { sessionId: "session-a" }).result
      .content[0].text,
    "compressed text",
  );
  assert.equal(
    postTool("exec", "call-a", "rtk output", { sessionId: "session-a" }),
    undefined,
  );
  assert.equal(
    postTool("exec", "call-a", "next output", { sessionId: "session-a" }).result
      .content[0].text,
    "compressed text",
  );
  assert.equal(
    postTool("exec", "call-b", "rtk output", { sessionId: "session-b" }),
    undefined,
  );

  const optimizations = requests().map(
    ({ request }) => request.input.output_optimization,
  );
  assert.deepEqual(optimizations, ["none", "rtk", "none", "rtk"]);
});

test("PostTool has no transcript hook and does not reprocess persistence", () => {
  assert.equal(handlers.has("tool_result_persist"), false);
  postTool("web_fetch", "call-once", "payload", { sessionId: "session-once" });
  assert.equal(requests().length, 1);
});

test("session mapping carries lifecycle attribution and session end clears state", () => {
  sessionStart({ sessionKey: "agent:main:one", sessionId: "session-mapped" });
  rewrite("mapped", {
    sessionKey: "agent:main:one",
    toolCallId: "call-mapped",
  });
  rmSync(requestLog, { force: true });

  postTool("exec", "call-mapped", "mapped output", {
    sessionKey: "agent:main:one",
  });
  let request = requests()[0].request;
  assert.equal(request.attribution.session_id, "session-mapped");
  assert.equal(request.input.output_optimization, "rtk");

  rewrite("cleanup", {
    sessionKey: "agent:main:one",
    toolCallId: "call-cleanup",
  });
  sessionEnd({ sessionKey: "agent:main:one" });
  rmSync(requestLog, { force: true });

  postTool("exec", "call-cleanup", "after end", {
    sessionId: "session-mapped",
  });
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

  postTool("exec", "call-existing", "rtk output", {
    sessionKey: "agent:main:existing",
  });
  const request = requests()[0].request;
  assert.equal(request.attribution.session_id, "session-existing");
  assert.equal(request.input.output_optimization, "rtk");
  // The recorded line is the one that ran, not the one the model wrote.
  assert.equal(request.input.command, "/mock/rtk resumed");
});

test("PostTool maps API and file origins without authorized recovery", () => {
  postTool("web_fetch", "call-api", "payload", { sessionId: "session-api" });
  assert.equal(requests()[0].request.input.content_origin, "api_response");
  assert.equal(
    requests()[0].request.input.capabilities.replace_with_text,
    true,
  );
  assert.equal(requests()[0].request.input.capabilities.recovery.kind, "none");
  rmSync(requestLog, { force: true });
  assert.equal(
    postTool("read", "call-read", "authoritative file", {
      sessionId: "session-read",
    }),
    undefined,
  );
  assert.equal(requests()[0].request.input.content_origin, "file_content");
});

test("PostTool replaces live text while preserving result metadata", () => {
  const message = {
    role: "toolResult",
    toolCallId: "call-envelope",
    toolName: "web_fetch",
    content: [{ type: "text", text: "compress me", mime: "text/plain" }],
    details: { source: "test" },
    timestamp: 42,
  };
  const result = postTool("web_fetch", "call-envelope", message, {
    sessionId: "session-envelope",
  });

  assert.deepEqual(result, {
    result: {
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

  assert.equal(postTool("image", "call-media", media), undefined);
  assert.equal(postTool("web_fetch", "call-multiple", multiple), undefined);
  assert.equal(requests().length, 0);
});

for (const length of [100000, 150000]) {
  test(`PostTool replaces only the supplied ${length}-character text`, () => {
    // The adapter cannot distinguish a host-clipped prefix from complete text.
    const source =
      "x".repeat(150000 - "TAIL_SENTINEL".length) + "TAIL_SENTINEL";
    const received = {
      content: [
        { type: "text", text: source.slice(0, length), mime: "text/plain" },
      ],
      details: { status: "completed", exitCode: 0, source: "host" },
      timestamp: 42,
    };
    const snapshot = structuredClone(received);
    const returned = postTool("exec", `long-${length}`, received);
    const input = requests()[0].request.input;
    assert.equal(input.content, received.content[0].text);
    assert.deepEqual(input.capabilities.recovery, { kind: "none" });
    assert.equal(returned.result.details, received.details);
    assert.deepEqual(returned.result, {
      ...received,
      content: [{ ...received.content[0], text: "compressed text" }],
    });
    assert.deepEqual(received, snapshot);
  });

  test(`PostTool failures preserve the received ${length}-character result`, () => {
    const received = {
      content: [{ type: "text", text: "x".repeat(length) }],
      details: { truncated: true },
    };
    const snapshot = structuredClone(received);
    for (const behavior of [
      "exit",
      "malformed",
      "wrong-version",
      "no-savings",
    ]) {
      writeFileSync(behaviorFile, behavior);
      assert.equal(postTool("exec", `failure-${length}`, received), undefined);
      assert.deepEqual(received, snapshot);
    }
    assert.equal(requests().length, 4);
    for (const { request } of requests()) {
      assert.equal(request.input.content, received.content[0].text);
    }
    rmSync(behaviorFile);
    assert.equal(
      postTool("read", `passthrough-${length}`, received),
      undefined,
    );
    assert.deepEqual(received, snapshot);
  });
}

test("PostTool appends Core diagnostics without replacing the tool error", () => {
  const message = {
    role: "toolResult",
    toolCallId: "call-error",
    toolName: "exec",
    isError: true,
    content: [{ type: "text", text: "command not found" }],
    details: { exitCode: 127 },
  };
  const result = postTool(
    "exec",
    "call-error",
    message,
    { sessionId: "session-error" },
    { isError: true },
  );

  assert.deepEqual(result, {
    result: {
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
  assert.equal(postTool("web_fetch", "call-lossy", "lossy payload"), undefined);
  assert.equal(requests()[0].request.input.capabilities.recovery.kind, "none");

  rmSync(requestLog, { force: true });
  writeFileSync(behaviorFile, "exit");
  assert.equal(postTool("web_fetch", "call-exit", "payload"), undefined);
  assert.equal(requests().length, 1);
});

test("PostTool propagates actual executed command and host failure metadata", () => {
  rewrite("old", { sessionId: "actual", toolCallId: "actual-call" });
  rmSync(requestLog, { force: true });
  postTool(
    "exec",
    "actual-call",
    "payload",
    { sessionId: "actual" },
    { args: { command: "cat actual.html" } },
  );
  assert.equal(requests()[0].request.input.command, "cat actual.html");

  for (const details of [
    { status: "failed" },
    { status: "timed_out" },
    { exitCode: 127 },
    { ok: false },
  ]) {
    rmSync(requestLog, { force: true });
    const original = { content: [{ type: "text", text: "failure" }], details };
    const returned = postTool("exec", "metadata-error", original);
    assert.equal(requests()[0].request.input.status, "error");
    assert.equal(returned.result.details, details);
    assert.equal(returned.result.content[0], original.content[0]);
  }
});

test("PostTool passes through malformed input, unusable responses and no savings", () => {
  for (const result of [
    null,
    [],
    {},
    { content: [null] },
    { content: [{ type: "text", text: 12 }] },
  ]) {
    assert.equal(postTool("exec", "invalid-input", result), undefined);
  }
  assert.equal(requests().length, 0);
  for (const behavior of [
    "malformed",
    "wrong-version",
    "invalid-output",
    "no-savings",
  ]) {
    writeFileSync(behaviorFile, behavior);
    assert.equal(postTool("web_fetch", "response-error", "payload"), undefined);
  }
});

test(
  "PostTool timeout returns unchanged output instead of throwing into the host",
  { timeout: 25000 },
  () => {
    writeFileSync(behaviorFile, "timeout");
    for (const length of [100000, 150000]) {
      const received = {
        content: [{ type: "text", text: "x".repeat(length) }],
        details: { source: "host" },
      };
      const snapshot = structuredClone(received);
      assert.equal(postTool("web_fetch", "timeout", received), undefined);
      assert.deepEqual(received, snapshot);
    }
  },
);

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
  postTool("exec", "call-old", "old output", { sessionId: "session-ttl" });
  assert.equal(requests()[0].request.input.output_optimization, "none");
});
