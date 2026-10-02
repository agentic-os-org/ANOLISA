#!/usr/bin/env node

import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { TokenlessPlugin } from "../adapters/tokenless/opencode/plugin.js";

const sandbox = mkdtempSync(join(tmpdir(), "tokenless-opencode-plugin-"));
const runner = join(sandbox, "hook runner.sh");
const log = join(sandbox, "hooks.log");

writeFileSync(runner, `#!/usr/bin/env bash
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
    printf '%s\\n' '{"hookSpecificOutput":{"updatedToolOutput":"compressed-response","additionalContext":"[tokenless:env] warning"}}'
    ;;
  compress_schema_hook.py)
    printf '%s\\n' '{"hookSpecificOutput":{"llm_request":{"config":{"tools":[{"name":"bash","description":"compressed description","parameters":{"type":"object","properties":{"command":{"type":"string"}}}}]}}}}'
    ;;
  *) printf '%s\\n' '{}' ;;
esac
`);

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
  await hooks["tool.execute.after"](
    { ...beforeInput, args: beforeOutput.args },
    afterOutput,
  );
  assert.equal(
    afterOutput.output,
    "[tokenless:ready] partial\n[tokenless:env] warning\ncompressed-response",
  );

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
  assert.equal(
    records.filter((record) => record.hook === "compress_schema_hook.py").length,
    1,
  );

  console.log("OpenCode plugin tests passed");
} finally {
  rmSync(sandbox, { recursive: true, force: true });
}
