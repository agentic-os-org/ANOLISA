// tests/smoke-test.ts
import { testCapability } from "./test-harness.js";
import { codeScan } from "../src/capabilities/code-scan.js";
import { observability } from "../src/capabilities/observability.js";
import { piiScan } from "../src/capabilities/pii-scan.js";
import { promptScan } from "../src/capabilities/prompt-scan.js";
import { skillLedger } from "../src/capabilities/skill-ledger.js";
import { _setCliMock } from "../src/utils.js";

// 每个 hook 的 mock 事件（字段与真实类型一致）
// Note: before_tool_call has two entries — one for exec-based tools (code-scan)
// and one for read-based tools (skill-ledger). The shared mock uses "exec" for
// code-scan / prompt-scan. skill-ledger uses its own dedicated mock events below.
const mockEvents: Record<string, Record<string, unknown>> = {
  before_tool_call: {
    toolName: "exec",
    params: { command: "ls -la" },
    runId: "run-001",
    sessionId: "session-001",
    toolCallId: "tc-001",
  },
  before_dispatch: {
    content: "hello world",
    body: "hello world",
    senderId: "user-123",
    isGroup: false,
  },
  before_agent_run: {
    prompt: "hello world",
    systemPrompt: "system prompt",
    messages: [{ role: "user", content: "earlier message" }],
  },
  before_prompt_build: {
    runId: "run-001",
    sessionId: "session-001",
    prompt: "hello world",
    messages: [{ role: "user", content: "hello world" }],
  },
  reply_dispatch: {
    runId: "run-001",
    sessionId: "session-001",
    sendPolicy: "allow",
    inboundAudio: false,
    shouldRouteToOriginating: false,
    shouldSendToolSummaries: true,
  },
  llm_input: {
    runId: "run-001",
    sessionId: "session-001",
    provider: "openai",
    model: "gpt-5.4",
    systemPrompt: "system prompt",
    prompt: "hello world",
    historyMessages: [{ role: "user", content: "hello" }],
    imagesCount: 0,
  },
  model_call_started: {
    runId: "run-001",
    callId: "call-001",
    sessionKey: "sk-001",
    sessionId: "session-001",
    provider: "openai",
    model: "gpt-5.4",
    api: "responses",
    transport: "http",
  },
  model_call_ended: {
    runId: "run-001",
    callId: "call-001",
    sessionKey: "sk-001",
    sessionId: "session-001",
    provider: "openai",
    model: "gpt-5.4",
    api: "responses",
    transport: "http",
    durationMs: 123,
    outcome: "completed",
    upstreamRequestIdHash: "hash-001",
  },
  llm_output: {
    runId: "run-001",
    sessionId: "session-001",
    provider: "openai",
    model: "gpt-5.4",
    resolvedRef: "openai/gpt-5.4",
    harnessId: "pi-embedded",
    assistantTexts: ["Hello."],
    lastAssistant: "Hello.",
    usage: { input: 10, output: 2, total: 12 },
  },
  agent_end: {
    runId: "run-001",
    success: true,
    durationMs: 321,
    messages: [
      { role: "user", content: [{ type: "text", text: "hello" }] },
      { role: "assistant", content: [{ type: "text", text: "Hello." }] },
    ],
  },
  after_tool_call: {
    toolName: "exec",
    params: { command: "ls -la" },
    runId: "run-001",
    sessionId: "session-001",
    toolCallId: "tc-001",
    result: { content: "ok" },
    durationMs: 20,
  },
};

// 每个 hook 的 mock ctx（提供代表性字段值）
const mockCtx: Record<string, Record<string, unknown>> = {
  before_tool_call: {
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
    toolName: "exec",
    toolCallId: "tc-001",
  },
  before_dispatch: {
    channelId: "telegram",
    sessionKey: "sk-001",
    senderId: "user-123",
  },
  before_prompt_build: {
    channelId: "telegram",
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
  },
  reply_dispatch: {
    dispatcher: {
      sendToolResult: () => false,
      sendBlockReply: () => true,
      sendFinalReply: () => false,
      waitForIdle: async () => {},
      getQueuedCounts: () => ({ tool: 0, block: 0, final: 0 }),
      getFailedCounts: () => ({ tool: 0, block: 0, final: 0 }),
      markComplete: () => {},
    },
    recordProcessed: () => {},
    markIdle: () => {},
  },
  llm_input: {
    channelId: "telegram",
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
  },
  model_call_started: {
    channelId: "telegram",
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
  },
  model_call_ended: {
    channelId: "telegram",
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
  },
  llm_output: {
    channelId: "telegram",
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
  },
  agent_end: {
    channelId: "telegram",
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
  },
  after_tool_call: {
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-001",
    toolName: "exec",
    toolCallId: "tc-001",
  },
};

const caps = [codeScan, promptScan, piiScan, observability];

if (!process.env.AGENT_SEC_LIVE) {
  _setCliMock(async (args) => {
    const offset = args[0] === "--trace-context" ? 2 : 0;
    if (args[offset] === "scan-code") {
      return {
        exitCode: 0,
        stdout: '{"verdict":"pass","findings":[]}',
        stderr: "",
      };
    }
    if (args[offset] === "scan-prompt") {
      return {
        exitCode: 0,
        stdout: '{"verdict":"pass","findings":[]}',
        stderr: "",
      };
    }
    if (args[offset] === "scan-pii") {
      return {
        exitCode: 0,
        stdout: '{"verdict":"pass","findings":[]}',
        stderr: "",
      };
    }
    if (
      args[offset] === "skill-ledger" &&
      args[offset + 1] === "init" &&
      args[offset + 2] === "--no-baseline"
    ) {
      return { exitCode: 0, stdout: '{"fingerprint":"mock"}', stderr: "" };
    }
    if (args[offset] === "skill-ledger" && args[offset + 1] === "check") {
      return { exitCode: 0, stdout: '{"status":"pass"}', stderr: "" };
    }
    return { exitCode: 0, stdout: "", stderr: "" };
  });
}

// skill-ledger needs a dedicated mock with read + SKILL.md path
const skillLedgerMockEvents: Record<string, Record<string, unknown>> = {
  ...mockEvents,
  before_tool_call: {
    toolName: "read",
    params: { file_path: "/home/user/.openclaw/skills/github/SKILL.md" },
    runId: "run-002",
    sessionId: "session-001",
    toolCallId: "tc-002",
  },
  reply_dispatch: {
    runId: "run-002",
    sessionId: "session-001",
    sendPolicy: "allow",
    inboundAudio: false,
    shouldRouteToOriginating: false,
    shouldSendToolSummaries: true,
  },
};
const skillLedgerMockCtx: Record<string, Record<string, unknown>> = {
  ...mockCtx,
  before_tool_call: {
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-002",
    toolName: "read",
    toolCallId: "tc-002",
  },
  reply_dispatch: {
    ...mockCtx.reply_dispatch,
    sessionKey: "sk-001",
    sessionId: "session-001",
    runId: "run-002",
  },
};

console.log("=== Agent-Sec Smoke Test ===");
console.log(
  `Mode: ${process.env.AGENT_SEC_LIVE ? "LIVE (real CLI)" : "MOCK (no CLI needed)"}\n`,
);

for (const cap of caps) {
  console.log(`[${cap.id}] hooks: [${cap.hooks.join(", ")}]`);
  const results = await testCapability(cap, mockEvents, undefined, mockCtx);
  for (const r of results) {
    const status = r.error ? `FAIL: ${r.error.message}` : "OK";
    const detail = r.result ? ` → ${JSON.stringify(r.result)}` : "";
    console.log(
      `  ${r.hookName}: ${status} (${r.durationMs.toFixed(0)}ms)${detail}`,
    );
  }
  console.log();
}

// ── skill-ledger (separate mock events) ──────────────────────────
console.log(`[${skillLedger.id}] hooks: [${skillLedger.hooks.join(", ")}]`);
const slResults = await testCapability(
  skillLedger,
  skillLedgerMockEvents,
  undefined,
  skillLedgerMockCtx,
);
for (const r of slResults) {
  const status = r.error ? `FAIL: ${r.error.message}` : "OK";
  const detail = r.result ? ` → ${JSON.stringify(r.result)}` : "";
  console.log(
    `  ${r.hookName}: ${status} (${r.durationMs.toFixed(0)}ms)${detail}`,
  );
}
console.log();
