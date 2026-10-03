#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { TokenlessPlugin } from "../adapters/tokenless/opencode/plugin.js";

const sandbox = mkdtempSync(join(tmpdir(), "tokenless-opencode-plugin-"));
const runner = join(sandbox, "hook runner.sh");
const log = join(sandbox, "hooks.log");

async function assertHookTimeoutCleansUp(mode) {
  const timeoutRunner = join(sandbox, `timeout-${mode}.sh`);
  const pidFile = join(sandbox, `timeout-${mode}.pid`);
  writeFileSync(
    timeoutRunner,
    `#!/usr/bin/env bash
cat >/dev/null
${mode === "ignore-term" ? "trap '' TERM" : ""}
sleep 60 &
sleeper=$!
printf '%s %s\\n' "$$" "$sleeper" > "$TOKENLESS_TEST_PID"
printf '{}'
${mode === "descendant" ? "exit 0" : 'wait "$sleeper"'}
`,
  );
  const pluginUrl = new URL("../adapters/tokenless/opencode/plugin.js", import.meta.url).href;
  const probe = `
    import { TokenlessPlugin } from ${JSON.stringify(pluginUrl)};
    const realSetTimeout = globalThis.setTimeout;
    globalThis.setTimeout = (callback, delay, ...args) =>
      realSetTimeout(callback, delay === 15_000 ? 500 : delay, ...args);
    const hooks = await TokenlessPlugin();
    const output = { args: {} };
    await hooks["tool.execute.before"](
      { tool: "read", sessionID: "timeout-session", callID: "timeout-call" }, output,
    );
    console.log(JSON.stringify(output));
  `;
  const livePids = [];
  try {
    const child = spawnSync(process.execPath, ["--input-type=module", "--eval", probe], {
      env: {
        ...process.env,
        TOKENLESS_HOOK_RUNNER: timeoutRunner,
        TOKENLESS_TEST_PID: pidFile,
      },
      encoding: "utf8",
      timeout: 3_000,
      killSignal: "SIGKILL",
    });
    const pids = readFileSync(pidFile, "utf8").trim().split(/\s+/).map(Number);
    const cleanupDeadline = Date.now() + 500;
    do {
      livePids.length = 0;
      for (const pid of pids) {
        try {
          process.kill(pid, 0);
          // A killed descendant may briefly await reaping by the system's init.
          const state =
            process.platform === "linux"
              ? readFileSync(`/proc/${pid}/stat`, "utf8").split(") ")[1].split(" ")[0]
              : "running";
          if (state !== "Z") livePids.push(pid);
        } catch (error) {
          if (error.code !== "ESRCH" && error.code !== "ENOENT") throw error;
        }
      }
      if (!livePids.length) break;
      await new Promise((resolve) => setTimeout(resolve, 10));
    } while (Date.now() < cleanupDeadline);
    assert.ifError(child.error);
    assert.equal(child.status, 0, child.stderr);
    assert.equal(child.stdout.trim(), '{"args":{}}');
    assert.deepEqual(livePids, [], `${mode} hook processes survived their timeout`);
  } finally {
    for (const pid of livePids) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {}
    }
  }
}

writeFileSync(
  runner,
  `#!/usr/bin/env bash
set -euo pipefail
hook="\${1:?hook name required}"
payload="$(cat)"
printf '%s\\t%s\\n' "$hook" "$payload" >> "$TOKENLESS_TEST_LOG"
case "$hook" in
  tool_ready_hook.sh)
    case "\${TOKENLESS_TEST_READY:-}" in
      block) printf '%s\\n' '{"decision":"block","reason":"missing required dependency"}' ;;
      partial) printf '%s\\n' '{"hookSpecificOutput":{"additionalContext":"[tokenless:ready] partial"}}' ;;
      *) printf '%s\\n' '{}' ;;
    esac
    ;;
  rewrite_hook.py)
    printf '%s\\n' '{"hookSpecificOutput":{"updatedInput":{"command":"rtk git status"}}}'
    ;;
  compress_response_hook.py)
    if [[ "$payload" == *'"is_error":true'* ]]; then
      printf '%s\\n' '{"hookSpecificOutput":{"additionalContext":"[tokenless:env] command failed"}}'
    else
      printf '%s\\n' '{"hookSpecificOutput":{"updatedToolOutput":"compressed-response","additionalContext":"[tokenless:env] warning"}}'
    fi
    ;;
  compress_schema_hook.py)
    printf '%s\\n' '{"hookSpecificOutput":{"llm_request":{"config":{"tools":[{"name":"bash","description":"compressed description","parameters":{"type":"object","properties":{"command":{"type":"string"}}}}]}}}}'
    ;;
  *) printf '%s\\n' '{}' ;;
esac
`,
);

process.env.TOKENLESS_HOOK_RUNNER = runner;
process.env.TOKENLESS_TEST_LOG = log;

try {
  const hooks = await TokenlessPlugin();
  process.env.TOKENLESS_TEST_READY = "partial";

  const beforeInput = { tool: "bash", sessionID: "session-1", callID: "call-1" };
  const beforeOutput = { args: { command: "git status" } };
  await hooks["tool.execute.before"](beforeInput, beforeOutput);
  assert.equal(beforeOutput.args.command, "rtk git status");

  const afterOutput = { title: "bash", output: "original-response", metadata: {} };
  await hooks["tool.execute.after"]({ ...beforeInput, args: beforeOutput.args }, afterOutput);
  assert.equal(
    afterOutput.output,
    "[tokenless:ready] partial\n[tokenless:env] warning\ncompressed-response",
  );

  const failureCases = [
    { tool: "bash", exit: 1, isError: true },
    { tool: "bash", exit: 127, isError: true },
    { tool: "bash", exit: -1, isError: true },
    { tool: "bash", exit: 0, isError: false },
    { tool: "bash", exit: undefined, isError: false },
    { tool: "bash", exit: null, isError: false },
    { tool: "bash", exit: "127", isError: false },
    { tool: "bash", exit: 1.5, isError: false },
    { tool: "api_call", exit: 127, isError: false },
  ];
  for (const [index, testCase] of failureCases.entries()) {
    const metadata = { exit: testCase.exit, description: "preserve metadata" };
    const original = "command not found: important failure details";
    const result = { title: "command", output: original, metadata };
    await hooks["tool.execute.after"](
      {
        tool: testCase.tool,
        sessionID: "failure-session",
        callID: `failure-${index}`,
        args: { command: "missing-command" },
      },
      result,
    );
    assert.equal(
      result.output,
      testCase.isError
        ? `[tokenless:env] command failed\n${original}`
        : "[tokenless:env] warning\ncompressed-response",
      `unexpected output for ${JSON.stringify(testCase)}`,
    );
    assert.equal(result.metadata, metadata);
    assert.equal(result.title, "command");
  }

  process.env.TOKENLESS_TEST_READY = "block";
  await assert.rejects(
    hooks["tool.execute.before"](
      { tool: "bash", sessionID: "session-1", callID: "call-2" },
      { args: { command: "cargo test" } },
    ),
    /missing required dependency/,
  );

  process.env.TOKENLESS_TEST_READY = "";
  const definition = {
    description: "A very long tool description",
    parameters: { type: "object", title: "Bash", properties: {} },
  };
  await hooks["tool.definition"]({ toolID: "bash" }, definition);
  assert.equal(definition.description, "compressed description");
  assert.deepEqual(definition.parameters, {
    type: "object",
    properties: { command: { type: "string" } },
  });

  const repeatedDefinition = {
    description: "A very long tool description",
    parameters: { type: "object", title: "Bash", properties: {} },
  };
  await hooks["tool.definition"]({ toolID: "bash" }, repeatedDefinition);
  assert.equal(repeatedDefinition.description, "compressed description");

  const circularArgs = {};
  circularArgs.self = circularArgs;
  await hooks["tool.execute.before"](
    { tool: "read", sessionID: "session-1", callID: "call-3" },
    { args: circularArgs },
  );

  const records = readFileSync(log, "utf8")
    .trim()
    .split("\n")
    .map((line) => {
      const separator = line.indexOf("\t");
      return {
        hook: line.slice(0, separator),
        payload: JSON.parse(line.slice(separator + 1)),
      };
    });
  const rewrite = records.find((record) => record.hook === "rewrite_hook.py");
  assert.equal(rewrite.payload.session_id, "session-1");
  assert.equal(rewrite.payload.tool_call_id, "call-1");
  assert.equal(rewrite.payload.tool_name, "bash");
  for (const [index, testCase] of failureCases.entries()) {
    const record = records.find((entry) => entry.payload.tool_call_id === `failure-${index}`);
    assert.equal(record.payload.is_error === true, testCase.isError);
    assert.equal(record.payload.tool_response, "command not found: important failure details");
  }
  assert.equal(records.filter((record) => record.hook === "compress_schema_hook.py").length, 1);

  if (process.platform !== "win32") {
    const failures = [];
    for (const mode of ["descendant", "ignore-term"]) {
      try {
        await assertHookTimeoutCleansUp(mode);
      } catch (error) {
        failures.push(new Error(`${mode}: ${error.message}`, { cause: error }));
      }
    }
    if (failures.length) throw new AggregateError(failures, "Hook cleanup failed");
  }

  console.log("OpenCode plugin tests passed");
} finally {
  rmSync(sandbox, { recursive: true, force: true });
}
