import { afterEach, describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  chmodSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { resolve } from "node:path";
import {
  buildTraceContext,
  callAgentSecCli,
  type TraceContext,
  _resetCliMock,
  _setCliMock,
} from "../../src/utils.js";

const _validTraceContextTypeCheck: TraceContext = {
  agent_name: "openclaw",
  trace_id: "trace-1",
  session_id: "session-1",
  run_id: "run-1",
  call_id: "call-1",
  tool_call_id: "tool-1",
};

// @ts-expect-error TraceContext intentionally rejects non-canonical keys.
const _invalidTraceContextTypeCheck: TraceContext = { sessionId: "session-1" };

describe("utils", () => {
  const originalPath = process.env.PATH;
  const originalCapturePath = process.env.AGENT_SEC_ARG_CAPTURE_PATH;
  const tempDirs: string[] = [];

  afterEach(() => {
    _resetCliMock();
    if (originalPath === undefined) {
      delete process.env.PATH;
    } else {
      process.env.PATH = originalPath;
    }
    if (originalCapturePath === undefined) {
      delete process.env.AGENT_SEC_ARG_CAPTURE_PATH;
    } else {
      process.env.AGENT_SEC_ARG_CAPTURE_PATH = originalCapturePath;
    }
    for (const dir of tempDirs.splice(0)) {
      rmSync(dir, { recursive: true, force: true });
    }
  });

  it("buildTraceContext accepts snake_case and camelCase with snake_case precedence", () => {
    const context = buildTraceContext(
      {
        sessionId: "camel-session",
        session_id: "snake-session",
        runId: "camel-run",
        run_id: "snake-run",
        traceId: "trace-1",
        agentName: "spoofed",
      },
      {},
    );

    assert.deepEqual(context, {
      agent_name: "openclaw",
      trace_id: "trace-1",
      session_id: "snake-session",
      run_id: "snake-run",
    });
  });

  it("buildTraceContext searches direct event before direct ctx and ignores nested trace objects", () => {
    const context = buildTraceContext(
      {
        sessionId: "event-session",
        runId: "event-run",
        toolCallId: "event-tool",
        trace: {
          traceId: "nested-event-trace",
          sessionId: "nested-event-session",
          runId: "nested-event-run",
          callId: "nested-event-call",
          toolUseId: "nested-event-tool",
        },
      },
      {
        trace_id: "direct-ctx-trace",
        session_id: "direct-ctx-session",
        run_id: "direct-ctx-run",
        trace: {
          run_id: "nested-ctx-run",
          call_id: "nested-ctx-call",
          tool_call_id: "nested-ctx-tool",
        },
      },
    );

    assert.deepEqual(context, {
      agent_name: "openclaw",
      trace_id: "direct-ctx-trace",
      session_id: "event-session",
      run_id: "event-run",
      tool_call_id: "event-tool",
    });
  });

  it("buildTraceContext ignores nested trace-only input except fixed agent name", () => {
    const context = buildTraceContext(
      {
        trace: {
          sessionId: "nested-session",
          runId: "nested-run",
          toolCallId: "nested-tool",
        },
      },
      {},
    );

    assert.deepEqual(context, { agent_name: "openclaw" });
  });

  it("buildTraceContext ignores empty and non-string values except fixed agent name", () => {
    const context = buildTraceContext(
      {
        trace_id: "",
        session_id: 123,
        run_id: "  ",
        call_id: null,
      },
      {},
    );

    assert.deepEqual(context, { agent_name: "openclaw" });
  });

  it("buildTraceContext always returns fixed OpenClaw agent name", () => {
    const context = buildTraceContext(
      { agent_name: "hermes", agentName: "cosh" },
      { agent_name: "spoofed" },
    );

    assert.deepEqual(context, { agent_name: "openclaw" });
  });

  it("callAgentSecCli injects trace context before the subcommand for mocks", async () => {
    let capturedArgs: string[] | undefined;
    _setCliMock(async (args) => {
      capturedArgs = args;
      return { stdout: "{}", stderr: "", exitCode: 0 };
    });

    await callAgentSecCli(["scan-code", "--code", "echo ok"], {
      traceContext: { session_id: "session-1", run_id: "run-1" },
    });

    assert.deepEqual(capturedArgs?.slice(0, 2), [
      "--trace-context",
      JSON.stringify({ session_id: "session-1", run_id: "run-1" }),
    ]);
    assert.equal(capturedArgs?.[2], "scan-code");
  });

  it("callAgentSecCli injects trace context before the subcommand for execFile", async () => {
    const tempDir = mkdtempSync(resolve(tmpdir(), "openclaw-utils-"));
    tempDirs.push(tempDir);
    const capturePath = resolve(tempDir, "args.json");
    const cliPath = resolve(tempDir, "agent-sec-cli");
    writeFileSync(
      cliPath,
      [
        `#!${process.execPath}`,
        "const fs = require('node:fs');",
        "fs.writeFileSync(process.env.AGENT_SEC_ARG_CAPTURE_PATH, JSON.stringify(process.argv.slice(2)));",
        "process.stdout.write('{}');",
      ].join("\n"),
    );
    chmodSync(cliPath, 0o755);
    process.env.PATH = tempDir;
    process.env.AGENT_SEC_ARG_CAPTURE_PATH = capturePath;

    const result = await callAgentSecCli(["scan-code", "--code", "echo ok"], {
      traceContext: { trace_id: "trace-1", tool_call_id: "tool-1" },
    });

    assert.equal(result.exitCode, 0);
    const capturedArgs = JSON.parse(readFileSync(capturePath, "utf8"));
    assert.deepEqual(capturedArgs.slice(0, 2), [
      "--trace-context",
      JSON.stringify({ trace_id: "trace-1", tool_call_id: "tool-1" }),
    ]);
    assert.equal(capturedArgs[2], "scan-code");
  });

  it("preserves spawn error details when agent-sec-cli cannot be started", async () => {
    process.env.PATH = "";

    const result = await callAgentSecCli(["observability", "record"], {
      timeout: 100,
    });

    assert.equal(result.exitCode, 1);
    assert.match(result.stderr, /agent-sec-cli/);
    assert.match(result.stderr, /ENOENT/);
  });
});
