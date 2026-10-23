#!/usr/bin/env node
/**
 * Behavioural tests for the Command Code (cmd) Tokenless mod.
 *
 * cmd compiles mods with jiti, so the shipped `tokenless.ts` deliberately
 * stays inside the plain ES module subset. That is the invariant this suite
 * depends on: it loads the shipped bytes verbatim under a `.mjs` name, with no
 * TypeScript loader, and drives them through a fake shared hook runner. A mod
 * that grows TypeScript-only syntax fails here before it ships.
 */

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const sandbox = mkdtempSync(join(tmpdir(), "tokenless-cmd-mod-"));
const adapterRoot = join(sandbox, "adapters", "tokenless");
const runner = join(sandbox, "hook runner.sh");
const log = join(sandbox, "hooks.log");

/** Load the shipped mod bytes as an ES module and return its namespace. */
async function loadMod() {
  const staged = join(sandbox, `tokenless-${Date.now()}-${Math.random().toString(36).slice(2)}.mjs`);
  copyFileSync(new URL("../adapters/tokenless/cmd/tokenless.ts", import.meta.url), staged);
  return import(pathToFileURL(staged).href);
}

writeFileSync(
  runner,
  `#!/usr/bin/env bash
set -euo pipefail
hook="\${1:?hook name required}"
payload="$(cat)"
printf '%s\\t%s\\tagent=%s\\n' "$hook" "$payload" "\${TOKENLESS_AGENT_ID:-unset}" >> "$TOKENLESS_TEST_LOG"
case "$hook" in
  tool_ready_hook.sh)
    case "\${TOKENLESS_TEST_READY:-}" in
      block) printf '%s\\n' '{"decision":"block","reason":"missing required dependency"}' ;;
      block-noreason) printf '%s\\n' '{"decision":"block"}' ;;
      partial) printf '%s\\n' '{"hookSpecificOutput":{"additionalContext":"[tokenless:ready] partial"}}' ;;
      *) printf '%s\\n' '{}' ;;
    esac
    ;;
  rewrite_hook.py)
    case "\${TOKENLESS_TEST_REWRITE:-}" in
      none) printf '%s\\n' '{}' ;;
      same) printf '%s\\n' '{"hookSpecificOutput":{"updatedInput":{"command":"git status"}}}' ;;
      *) printf '%s\\n' '{"hookSpecificOutput":{"updatedInput":{"command":"rtk git status"}}}' ;;
    esac
    ;;
  compress_response_hook.py)
    case "\${TOKENLESS_TEST_RESPONSE:-}" in
      empty) printf '%s\\n' '{}' ;;
      context) printf '%s\\n' '{"hookSpecificOutput":{"additionalContext":"[tokenless:env] warning"}}' ;;
      *) printf '%s\\n' '{"hookSpecificOutput":{"updatedToolOutput":"compressed-response","additionalContext":"[tokenless:env] warning"}}' ;;
    esac
    ;;
  *) printf '%s\\n' '{}' ;;
esac
`,
);
mkdirSync(join(adapterRoot, "common", "hooks"), { recursive: true });
copyFileSync(runner, join(adapterRoot, "common", "hooks", "run-hook.sh"));

function hookCalls() {
  try {
    return readFileSync(log, "utf8")
      .split("\n")
      .filter(Boolean)
      .map((line) => {
        const [hook, payload, agent] = line.split("\t");
        return { hook, payload: JSON.parse(payload), agent: agent.replace(/^agent=/, "") };
      });
  } catch {
    return [];
  }
}

function clearLog() {
  writeFileSync(log, "");
}

const mod = await loadMod();
const hooks = mod.createTokenlessHooks({ runner });

try {
  // -- the mod factory cmd requires -----------------------------------------
  assert.equal(typeof mod.default, "function", "cmd needs a default-export factory");
  assert.equal(mod.AGENT_ID, "cmd", "stats attribution must be agent=cmd");

  const registered = [];
  mod.default({
    hooks(value) {
      registered.push(value);
      return { dispose() {} };
    },
  });
  assert.equal(registered.length, 1, "the factory registers its hooks once");
  assert.deepEqual(
    Object.keys(registered[0]).sort(),
    ["afterToolCall", "beforeToolCall"],
    "cmd's ModApi exposes exactly these two mutating tool hooks",
  );

  // -- hook runner discovery -------------------------------------------------
  const discovered = spawnSync(
    process.execPath,
    ["--input-type=module", "--eval", `
      import { copyFileSync, mkdirSync } from "node:fs";
      mkdirSync(${JSON.stringify(join(adapterRoot, "cmd"))}, { recursive: true });
      copyFileSync(
        ${JSON.stringify(new URL("../adapters/tokenless/cmd/tokenless.ts", import.meta.url).pathname)},
        ${JSON.stringify(join(adapterRoot, "cmd", "tokenless.mjs"))},
      );
      const placed = await import(${JSON.stringify(pathToFileURL(join(adapterRoot, "cmd", "tokenless.mjs")).href)});
      console.log(placed.findHookRunner() ?? "null");
    `],
    {
      encoding: "utf8",
      env: { ...process.env, TOKENLESS_HOOK_RUNNER: "", ANOLISA_ADAPTER_DIR: "", HOME: sandbox },
    },
  );
  assert.equal(discovered.status, 0, discovered.stderr);
  assert.equal(
    discovered.stdout.trim(),
    join(adapterRoot, "common", "hooks", "run-hook.sh"),
    "the mod finds the shared runner beside it in the adapter tree",
  );

  // -- beforeToolCall: RTK rewrite ------------------------------------------
  process.env.TOKENLESS_TEST_LOG = log;
  clearLog();
  const rewritten = await hooks.beforeToolCall({
    toolCallId: "call-1",
    toolName: "shell_command",
    input: { command: "git status", description: "check the tree", timeout: 30000 },
    state: { sessionId: "session-1" },
  });
  assert.equal(rewritten.input.command, "rtk git status");
  assert.equal(
    rewritten.input.description,
    "check the tree",
    "cmd replaces the input wholesale, so sibling fields must survive",
  );
  assert.equal(rewritten.input.timeout, 30000);
  assert.equal(rewritten.block, undefined, "a rewrite must not block the call");

  const calls = hookCalls();
  assert.deepEqual(
    calls.map((call) => call.hook),
    ["tool_ready_hook.sh", "rewrite_hook.py"],
    "readiness runs before the rewrite",
  );
  assert.ok(
    calls.every((call) => call.agent === "cmd"),
    "every hook invocation is attributed to agent=cmd",
  );
  assert.deepEqual(
    {
      session_id: calls[1].payload.session_id,
      tool_use_id: calls[1].payload.tool_use_id,
      tool_call_id: calls[1].payload.tool_call_id,
      tool_name: calls[1].payload.tool_name,
      tool_input: calls[1].payload.tool_input,
    },
    {
      session_id: "session-1",
      tool_use_id: "call-1",
      tool_call_id: "call-1",
      tool_name: "shell_command",
      tool_input: { command: "git status", description: "check the tree", timeout: 30000 },
    },
    "the protocol payload carries cmd's tool call identity",
  );

  // Windows' native shell tool takes the same rewrite path.
  clearLog();
  const powershell = await hooks.beforeToolCall({
    toolCallId: "call-ps",
    toolName: "powershell",
    input: { command: "git status" },
    state: { sessionId: "session-1" },
  });
  assert.equal(powershell.input.command, "rtk git status");

  // -- beforeToolCall: nothing to say ---------------------------------------
  clearLog();
  assert.equal(
    await hooks.beforeToolCall({
      toolCallId: "call-2",
      toolName: "read_file",
      input: { file_path: "/repo/README.md" },
      state: { sessionId: "session-1" },
    }),
    undefined,
    "a non-shell tool never reaches the rewrite hook",
  );
  assert.deepEqual(
    hookCalls().map((call) => call.hook),
    ["tool_ready_hook.sh"],
    "only readiness runs for a non-shell tool",
  );

  clearLog();
  process.env.TOKENLESS_TEST_REWRITE = "none";
  assert.equal(
    await hooks.beforeToolCall({
      toolCallId: "call-3",
      toolName: "shell_command",
      input: { command: "git status" },
      state: { sessionId: "session-1" },
    }),
    undefined,
    "a hook with no rewrite leaves the call alone",
  );
  process.env.TOKENLESS_TEST_REWRITE = "same";
  assert.equal(
    await hooks.beforeToolCall({
      toolCallId: "call-4",
      toolName: "shell_command",
      input: { command: "git status" },
      state: { sessionId: "session-1" },
    }),
    undefined,
    "an unchanged command is not reported as a rewrite",
  );
  delete process.env.TOKENLESS_TEST_REWRITE;

  // A shell tool without a command string (a background task id, say) is left
  // alone rather than rewritten into something cmd cannot run.
  clearLog();
  assert.equal(
    await hooks.beforeToolCall({
      toolCallId: "call-5",
      toolName: "shell_command",
      input: { run_in_background: true },
      state: { sessionId: "session-1" },
    }),
    undefined,
  );
  assert.deepEqual(
    hookCalls().map((call) => call.hook),
    ["tool_ready_hook.sh"],
  );

  // -- beforeToolCall: readiness --------------------------------------------
  process.env.TOKENLESS_TEST_READY = "block";
  const blocked = await hooks.beforeToolCall({
    toolCallId: "call-6",
    toolName: "shell_command",
    input: { command: "git status" },
    state: { sessionId: "session-1" },
  });
  assert.equal(blocked.block, true);
  assert.equal(blocked.additionalContext, "missing required dependency");
  assert.equal(blocked.input, undefined, "a blocked call carries no rewrite");

  process.env.TOKENLESS_TEST_READY = "block-noreason";
  const blockedSilently = await hooks.beforeToolCall({
    toolCallId: "call-7",
    toolName: "shell_command",
    input: { command: "git status" },
    state: { sessionId: "session-1" },
  });
  assert.equal(blockedSilently.block, true);
  assert.match(blockedSilently.additionalContext, /not ready/);

  process.env.TOKENLESS_TEST_READY = "partial";
  clearLog();
  const advised = await hooks.beforeToolCall({
    toolCallId: "call-8",
    toolName: "shell_command",
    input: { command: "git status" },
    state: { sessionId: "session-1" },
  });
  assert.equal(advised.additionalContext, "[tokenless:ready] partial");
  assert.equal(advised.input.command, "rtk git status", "advice and rewrite compose");
  delete process.env.TOKENLESS_TEST_READY;

  // -- afterToolCall: response compression ----------------------------------
  clearLog();
  const compressed = await hooks.afterToolCall({
    toolCallId: "call-9",
    toolName: "shell_command",
    input: { command: "cat build.log" },
    result: [{ type: "text", text: "original-response" }],
    isError: false,
    state: { sessionId: "session-1" },
  });
  assert.deepEqual(compressed.content, [
    { type: "text", text: "compressed-response" },
  ]);
  assert.equal(compressed.additionalContext, "[tokenless:env] warning");
  const [postCall] = hookCalls();
  assert.equal(postCall.hook, "compress_response_hook.py");
  assert.equal(postCall.agent, "cmd");
  assert.equal(
    postCall.payload.tool_response,
    "original-response",
    "Core measures the model-visible text, not the block envelope",
  );
  assert.equal(postCall.payload.is_error, undefined);

  // A plain-string result keeps its shape.
  const stringResult = await hooks.afterToolCall({
    toolCallId: "call-10",
    toolName: "web_fetch",
    input: {},
    result: "original-response",
    state: { sessionId: "session-1" },
  });
  assert.equal(stringResult.content, "compressed-response");

  // Non-text blocks keep their place; folded text lands in the first slot.
  const mixed = await hooks.afterToolCall({
    toolCallId: "call-11",
    toolName: "read_file",
    input: { file_path: "/repo/a.png" },
    result: [
      { type: "image", source: { data: "aaa" } },
      { type: "text", text: "first" },
      { type: "text", text: "second" },
    ],
    state: { sessionId: "session-1" },
  });
  assert.deepEqual(mixed.content, [
    { type: "image", source: { data: "aaa" } },
    { type: "text", text: "compressed-response" },
  ]);

  // A failure keeps its diagnostic channel: the hook decides what to add.
  process.env.TOKENLESS_TEST_RESPONSE = "context";
  const failed = await hooks.afterToolCall({
    toolCallId: "call-12",
    toolName: "shell_command",
    input: { command: "missing-command" },
    result: [{ type: "text", text: "command not found" }],
    isError: true,
    state: { sessionId: "session-1" },
  });
  assert.equal(failed.content, undefined, "diagnostics-only replies replace nothing");
  assert.equal(failed.additionalContext, "[tokenless:env] warning");
  clearLog();
  await hooks.afterToolCall({
    toolCallId: "call-13",
    toolName: "shell_command",
    input: { command: "missing-command" },
    result: [{ type: "text", text: "command not found" }],
    isError: true,
    state: { sessionId: "session-1" },
  });
  assert.equal(
    hookCalls()[0].payload.is_error,
    true,
    "cmd's isError reaches the hook as is_error",
  );
  delete process.env.TOKENLESS_TEST_RESPONSE;

  // A result with no text is never sent anywhere.
  clearLog();
  assert.equal(
    await hooks.afterToolCall({
      toolCallId: "call-14",
      toolName: "read_file",
      input: {},
      result: [{ type: "image", source: { data: "aaa" } }],
      state: { sessionId: "session-1" },
    }),
    undefined,
  );
  assert.equal(
    await hooks.afterToolCall({
      toolCallId: "call-15",
      toolName: "read_file",
      input: {},
      result: null,
      state: { sessionId: "session-1" },
    }),
    undefined,
  );
  assert.deepEqual(hookCalls(), [], "a result with no text calls no hook");

  // -- session identity ------------------------------------------------------
  clearLog();
  const stateless = await hooks.afterToolCall({
    toolCallId: "call-16",
    toolName: "shell_command",
    input: { command: "true" },
    result: "text",
  });
  assert.ok(stateless, "a call without AgentState still compresses");
  const fallbackSession = hookCalls()[0].payload.session_id;
  assert.ok(fallbackSession, "the session id is never empty");
  clearLog();
  await hooks.afterToolCall({
    toolCallId: "call-17",
    toolName: "shell_command",
    input: { command: "true" },
    result: "text",
  });
  assert.equal(
    hookCalls()[0].payload.session_id,
    fallbackSession,
    "the pre/post pair must agree on the session that keys the RTK state",
  );

  // -- fail open ------------------------------------------------------------
  process.env.TOKENLESS_TEST_RESPONSE = "empty";
  assert.equal(
    await hooks.afterToolCall({
      toolCallId: "call-18",
      toolName: "shell_command",
      input: { command: "true" },
      result: "text",
      state: { sessionId: "session-1" },
    }),
    undefined,
    "an empty hook reply is no opinion",
  );
  delete process.env.TOKENLESS_TEST_RESPONSE;

  const noRunner = mod.createTokenlessHooks({ runner: null });
  assert.equal(
    await noRunner.beforeToolCall({
      toolCallId: "call-19",
      toolName: "shell_command",
      input: { command: "git status" },
      state: { sessionId: "session-1" },
    }),
    undefined,
    "no Tokenless install means no rewrite",
  );
  assert.equal(
    await noRunner.afterToolCall({
      toolCallId: "call-20",
      toolName: "shell_command",
      input: { command: "git status" },
      result: "text",
      state: { sessionId: "session-1" },
    }),
    undefined,
    "no Tokenless install means no compression",
  );

  const failingRunner = join(sandbox, "failing runner.sh");
  writeFileSync(failingRunner, "#!/usr/bin/env bash\ncat >/dev/null\nexit 3\n");
  const failing = mod.createTokenlessHooks({ runner: failingRunner });
  assert.equal(
    await failing.afterToolCall({
      toolCallId: "call-21",
      toolName: "shell_command",
      input: { command: "git status" },
      result: "text",
      state: { sessionId: "session-1" },
    }),
    undefined,
    "a failing hook leaves the result alone",
  );

  const garbageRunner = join(sandbox, "garbage runner.sh");
  writeFileSync(garbageRunner, "#!/usr/bin/env bash\ncat >/dev/null\necho 'not json'\n");
  const garbage = mod.createTokenlessHooks({ runner: garbageRunner });
  assert.equal(
    await garbage.beforeToolCall({
      toolCallId: "call-22",
      toolName: "shell_command",
      input: { command: "git status" },
      state: { sessionId: "session-1" },
    }),
    undefined,
    "an undecodable hook reply leaves the input alone",
  );

  // A hook that never returns must not pin the host: the timeout kills the
  // whole detached group and the call proceeds untouched.
  if (process.platform !== "win32") {
    const slowRunner = join(sandbox, "slow runner.sh");
    writeFileSync(
      slowRunner,
      "#!/usr/bin/env bash\ncat >/dev/null\nsleep 60 &\nsleeper=$!\nprintf '%s %s\\n' \"$$\" \"$sleeper\" > \"$TOKENLESS_TEST_PID\"\nsleep 60\n",
    );
    const pidFile = join(sandbox, "slow.pid");
    const probe = `
      import { copyFileSync } from "node:fs";
      copyFileSync(
        ${JSON.stringify(new URL("../adapters/tokenless/cmd/tokenless.ts", import.meta.url).pathname)},
        ${JSON.stringify(join(sandbox, "slow-probe.mjs"))},
      );
      const placed = await import(${JSON.stringify(pathToFileURL(join(sandbox, "slow-probe.mjs")).href)});
      const realSetTimeout = globalThis.setTimeout;
      globalThis.setTimeout = (callback, delay, ...args) =>
        realSetTimeout(callback, delay === 15_000 ? 500 : delay, ...args);
      const slow = placed.createTokenlessHooks({ runner: ${JSON.stringify(slowRunner)} });
      const out = await slow.afterToolCall({
        toolCallId: "call-slow", toolName: "shell_command",
        input: { command: "git status" }, result: "text", state: { sessionId: "s" },
      });
      console.log(JSON.stringify(out ?? null));
    `;
    const child = spawnSync(process.execPath, ["--input-type=module", "--eval", probe], {
      encoding: "utf8",
      env: { ...process.env, TOKENLESS_TEST_PID: pidFile },
      timeout: 10_000,
      killSignal: "SIGKILL",
    });
    assert.ifError(child.error);
    assert.equal(child.status, 0, child.stderr);
    assert.equal(child.stdout.trim(), "null", "a timed-out hook is no opinion");
    const pids = readFileSync(pidFile, "utf8").trim().split(/\s+/).map(Number);
    const deadline = Date.now() + 2_000;
    let live = [];
    do {
      live = pids.filter((pid) => {
        try {
          process.kill(pid, 0);
          return true;
        } catch {
          return false;
        }
      });
      if (!live.length) break;
      await new Promise((resolve) => setTimeout(resolve, 20));
    } while (Date.now() < deadline);
    for (const pid of live) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {}
    }
    assert.deepEqual(live, [], "the timed-out hook process group survived cleanup");
  }

  console.log("Command Code mod tests passed");
} finally {
  rmSync(sandbox, { recursive: true, force: true });
}
