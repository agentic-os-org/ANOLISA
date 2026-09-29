import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { piiScan } from "../../src/capabilities/pii-scan.js";
import { _setCliMock, _resetCliMock } from "../../src/utils.js";
import type { CliCallOptions, CliResult } from "../../src/utils.js";

type RegisteredHook = {
  hookName: string;
  handler: (event: any, ctx?: any) => Promise<any>;
  priority: number;
};

function createMockApi(pluginConfig: Record<string, any> = {}) {
  const hooks: RegisteredHook[] = [];
  const logs: string[] = [];
  const api = {
    pluginConfig,
    logger: {
      info: (msg: string) => logs.push(`[INFO] ${msg}`),
      error: (msg: string) => logs.push(`[ERROR] ${msg}`),
      warn: (msg: string) => logs.push(`[WARN] ${msg}`),
      debug: (msg: string) => logs.push(`[DEBUG] ${msg}`),
    },
    on: (hookName: string, handler: any, opts?: { priority?: number }) => {
      hooks.push({ hookName, handler, priority: opts?.priority ?? 0 });
    },
  };
  return { api: api as any, hooks, logs };
}

function registerHandlersWithoutDebug(pluginConfig: Record<string, any> = {}) {
  const { api, hooks, logs } = createMockApi(pluginConfig);
  delete api.logger.debug;
  piiScan.register(api);
  const beforeDispatch = hooks.find((hook) => hook.hookName === "before_dispatch");
  assert.ok(beforeDispatch, "before_dispatch handler should be registered");
  return { beforeDispatch, hooks, logs };
}

function registerHandlers(pluginConfig: Record<string, any> = {}) {
  const { api, hooks, logs } = createMockApi(pluginConfig);
  piiScan.register(api);
  const beforeDispatch = hooks.find((hook) => hook.hookName === "before_dispatch");
  assert.ok(beforeDispatch, "before_dispatch handler should be registered");
  return { beforeDispatch, hooks, logs };
}

function enableBlockConfig(enableBlock: boolean): Record<string, any> {
  return {
    capabilities: {
      "pii-scan-user-input": { enableBlock },
    },
  };
}

function policyConfig(policy: "observe" | "warn" | "ask" | "block"): Record<string, any> {
  return { capabilities: { "pii-scan-user-input": { policy } } };
}

let lastCliArgs: string[] | undefined;
let lastCliOpts: CliCallOptions | undefined;

function mockCli(result: CliResult) {
  _setCliMock(async (args, opts) => {
    lastCliArgs = args;
    lastCliOpts = opts;
    return result;
  });
}

function mockCliNoCall() {
  _setCliMock(async () => {
    throw new Error("CLI should not have been called");
  });
}

function scanResult(verdict: string, findings: unknown[]) {
  return {
    exitCode: 0,
    stdout: JSON.stringify({ verdict, findings }),
    stderr: "",
  };
}

const warnFinding = {
  type: "email",
  severity: "warn",
  evidence_redacted: "a***@example.com",
  raw_evidence: "alice@example.com",
};

const denyFinding = {
  type: "credential",
  severity: "deny",
  evidence_redacted: "password=[REDACTED]",
  raw_evidence: "password=secret",
};

describe("pii-scan-user-input", () => {
  beforeEach(() => {
    delete process.env.PII_CHECKER_HOOK_ENABLED;
    delete process.env.PII_CHECKER_MODE;
    lastCliArgs = undefined;
    lastCliOpts = undefined;
  });

  afterEach(() => {
    delete process.env.PII_CHECKER_HOOK_ENABLED;
    delete process.env.PII_CHECKER_MODE;
    _resetCliMock();
  });

  it("registers all PII scan hooks before prompt-scan priority", () => {
    const { hooks } = registerHandlers();

    assert.deepEqual(
      hooks.map((hook) => hook.hookName),
      ["before_dispatch", "before_tool_call", "after_tool_call", "llm_output"],
    );
    assert.deepEqual(piiScan.hooks, [
      "before_dispatch",
      "before_tool_call",
      "after_tool_call",
      "llm_output",
    ]);
    assert.equal(hooks[0].priority, 200);
  });

  it("does not call CLI for empty inbound text", async () => {
    const { beforeDispatch } = registerHandlers();
    mockCliNoCall();

    const result = await beforeDispatch.handler({ content: "   ", body: "   " });

    assert.equal(result, undefined);
  });

  it("passes scan-pii args and timeout", async () => {
    const { beforeDispatch } = registerHandlers();
    mockCli(scanResult("pass", []));

    await beforeDispatch.handler({ content: "hello", body: "fallback" });

    assert.deepEqual(lastCliArgs?.slice(0, 2), [
      "--trace-context",
      JSON.stringify({ agent_name: "openclaw" }),
    ]);
    assert.deepEqual(lastCliArgs?.slice(2), [
      "scan-pii",
      "--stdin",
      "--format",
      "json",
      "--redact-output",
      "--source",
      "user_input",
    ]);
    assert.equal(lastCliOpts?.timeout, 10000);
    assert.equal(lastCliOpts?.stdin, "hello");
  });

  it("falls back to body when content is empty", async () => {
    const { beforeDispatch } = registerHandlers();
    mockCli(scanResult("pass", []));

    await beforeDispatch.handler({ content: "   ", body: "hello from body" });

    assert.equal(lastCliOpts?.stdin, "hello from body");
  });

  it("adds --include-low-confidence when configured", async () => {
    const { beforeDispatch } = registerHandlers({ piiIncludeLowConfidence: true });
    mockCli(scanResult("pass", []));

    await beforeDispatch.handler({ content: "hello" });

    assert.ok(lastCliArgs?.includes("--include-low-confidence"));
  });

  it("pass verdict allows silently", async () => {
    const { beforeDispatch } = registerHandlers();
    mockCli(scanResult("pass", []));

    const result = await beforeDispatch.handler({ content: "hello" });

    assert.equal(result, undefined);
  });

  for (const enableBlock of [false, true]) {
    it(`warn verdict logs and allows when enableBlock=${enableBlock}`, async () => {
      const { beforeDispatch, logs } = registerHandlers(enableBlockConfig(enableBlock));
      mockCli(scanResult("warn", [warnFinding]));

      const result = await beforeDispatch.handler({ content: "email alice@example.com" });

      assert.equal(result, undefined);
      assert.ok(logs.some((log) => log.includes("[WARN] [pii-checker] 检测到")));
      assert.ok(!logs.some((log) => log.startsWith("[WARN]") && log.includes("verdict=")));
      assert.ok(logs.some((log) => log.includes("检测到 1 项一般风险敏感信息")));
      assert.ok(logs.some((log) => log.includes("本次仅提醒，未触发确认或阻断")));
      assert.ok(!logs.some((log) => log.includes("a***@example.com")));
      assert.ok(!logs.some((log) => log.includes("alice@example.com")));
    });
  }

  it("deny verdict defaults to observe without a user-visible warning", async () => {
    const { beforeDispatch, logs } = registerHandlers();
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result, undefined);
    assert.ok(!logs.some((log) => log.includes("[pii-checker] DENY")));
  });

  it("deny verdict blocks when enableBlock=true and omits raw evidence", async () => {
    const { beforeDispatch } = registerHandlers(enableBlockConfig(true));
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result?.handled, true);
    assert.match(result?.text, /\[pii-checker\]/);
    assert.match(result?.text, /检测到 1 项高风险敏感信息/);
    assert.match(result?.text, /当前策略已阻断本次请求/);
    assert.doesNotMatch(result?.text, /credential/);
    assert.doesNotMatch(result?.text, /password=\[REDACTED\]/);
    assert.doesNotMatch(result?.text, /password=secret/);
    assert.doesNotMatch(result?.text, /raw_evidence/);
  });

  it("block policy works when the host logger has no debug method", async () => {
    const { beforeDispatch } = registerHandlersWithoutDebug(policyConfig("block"));
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result?.handled, true);
    assert.match(result?.text, /当前策略已阻断本次请求/);
  });

  it("uses complete counts when finding details are reduced", async () => {
    const { beforeDispatch } = registerHandlers(policyConfig("block"));
    mockCli({
      exitCode: 0,
      stderr: "",
      stdout: JSON.stringify({
        verdict: "deny",
        findings: [denyFinding],
        summary: {
          total: 20001,
          by_severity: { deny: 1, warn: 20000 },
          findings_truncated: true,
        },
      }),
    });
    const result = await beforeDispatch.handler({ content: "password=secret" });
    assert.equal(result?.handled, true);
    assert.match(result?.text, /20001.*高风险 1、一般风险 20000.*明细已省略/);
  });

  it("summarizes mixed findings by per-finding risk", async () => {
    const { beforeDispatch } = registerHandlers(policyConfig("block"));
    mockCli(
      scanResult("deny", [
        warnFinding,
        denyFinding,
        {
          type: "custom",
          severity: "unknown",
          evidence_redacted: "custom-***",
        },
      ]),
    );

    const result = await beforeDispatch.handler({ content: "sensitive input" });

    assert.equal(result?.handled, true);
    assert.match(result?.text, /检测到 3 项敏感信息（高风险 2、一般风险 1）/);
    assert.doesNotMatch(result?.text, /email|credential|custom/);
    assert.doesNotMatch(result?.text, /warn|deny|unknown/);
    assert.doesNotMatch(result?.text, /\[REDACTED\]|a\*\*\*|custom-\*\*\*/);
  });

  it("block policy blocks a before_tool_call deny verdict", async () => {
    const { hooks } = registerHandlers(policyConfig("block"));
    const beforeToolCall = hooks.find((hook) => hook.hookName === "before_tool_call");
    assert.ok(beforeToolCall);
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeToolCall.handler(
      {
        toolName: "exec",
        params: { command: "password=secret" },
        sessionId: "session-1",
        toolCallId: "tool-1",
      },
      {},
    );

    assert.equal(result?.block, true);
    assert.match(result?.blockReason, /\[pii-checker\]/);
    assert.match(result?.blockReason, /检测到 1 项高风险敏感信息/);
    assert.match(result?.blockReason, /当前策略已阻断本次工具调用/);
    assert.doesNotMatch(result?.blockReason, /credential/);
    assert.doesNotMatch(result?.blockReason, /password=\[REDACTED\]/);
    assert.doesNotMatch(result?.blockReason, /password=secret/);
    assert.deepEqual(lastCliArgs, [
      "--trace-context",
      JSON.stringify({
        agent_name: "openclaw",
        session_id: "session-1",
        tool_call_id: "tool-1",
      }),
      "scan-pii",
      "--stdin",
      "--format",
      "json",
      "--redact-output",
      "--source",
      "tool_input",
    ]);
    assert.equal(lastCliOpts?.stdin, '{"command":"password=secret"}');
  });

  it("ask policy requests approval for a before_tool_call deny verdict", async () => {
    const { hooks } = registerHandlers(policyConfig("ask"));
    const beforeToolCall = hooks.find((hook) => hook.hookName === "before_tool_call");
    assert.ok(beforeToolCall);
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeToolCall.handler(
      {
        toolName: "exec",
        params: { command: "password=secret" },
      },
      {},
    );

    assert.equal(result?.requireApproval?.title, "PII Checker Security Review");
    assert.equal(result?.requireApproval?.severity, "critical");
    assert.match(result?.requireApproval?.description, /检测到 1 项高风险敏感信息/);
    assert.match(result?.requireApproval?.description, /当前策略要求确认，请确认后继续/);
    assert.doesNotMatch(result?.requireApproval?.description, /credential/);
    assert.doesNotMatch(result?.requireApproval?.description, /password=\[REDACTED\]/);
    assert.doesNotMatch(result?.requireApproval?.description, /password=secret/);
    assert.doesNotMatch(result?.requireApproval?.description, /仅提醒|继续处理/);
  });

  it("ask policy falls back to a warning before dispatch", async () => {
    const { beforeDispatch, logs } = registerHandlers(policyConfig("ask"));
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result, undefined);
    assert.ok(logs.some((log) => log.includes("verdict=deny policy=ask")));
    assert.ok(!logs.some((log) => log.startsWith("[WARN]") && log.includes("policy=")));
    assert.ok(logs.some((log) => log.includes("当前环节不支持确认/阻断，本次仅提醒，不会阻断")));
    assert.ok(!logs.some((log) => log.includes("password=[REDACTED]")));
    assert.ok(!logs.some((log) => log.includes("当前策略已阻断本次请求")));
  });

  it("after_tool_call logs warning without raw evidence", async () => {
    const { hooks, logs } = registerHandlers(policyConfig("warn"));
    const afterToolCall = hooks.find((hook) => hook.hookName === "after_tool_call");
    assert.ok(afterToolCall);
    mockCli(scanResult("warn", [warnFinding]));

    const result = await afterToolCall.handler(
      {
        result: { content: "email alice@example.com" },
        sessionId: "session-1",
        toolCallId: "tool-1",
      },
      {},
    );

    assert.equal(result, undefined);
    assert.ok(logs.some((log) => log.includes("[WARN] [pii-checker] 检测到")));
    assert.ok(logs.some((log) => log.includes("检测到 1 项一般风险敏感信息")));
    assert.ok(logs.some((log) => log.includes("工具已经执行")));
    assert.ok(logs.some((log) => log.includes("未触发确认或阻断")));
    assert.ok(!logs.some((log) => log.includes("当前环节不支持")));
    assert.ok(logs.some((log) => log.includes("工具结果仍会进入模型上下文")));
    assert.ok(logs.some((log) => log.includes("已发生的外部副作用不会撤销")));
    assert.ok(!logs.some((log) => log.includes("a***@example.com")));
    assert.ok(!logs.some((log) => log.includes("alice@example.com")));
    assert.equal(lastCliArgs?.at(-1), "tool_output");
  });

  it("block policy falls back to a warning after tool execution", async () => {
    const { hooks, logs } = registerHandlers(policyConfig("block"));
    const afterToolCall = hooks.find((hook) => hook.hookName === "after_tool_call");
    assert.ok(afterToolCall);
    mockCli(scanResult("deny", [denyFinding]));

    const result = await afterToolCall.handler(
      {
        result: { content: "password=secret" },
        sessionId: "session-1",
      },
      {},
    );

    assert.equal(result, undefined);
    assert.ok(logs.some((log) => log.includes("verdict=deny policy=block")));
    assert.ok(!logs.some((log) => log.startsWith("[WARN]") && log.includes("policy=")));
    assert.ok(logs.some((log) => log.includes("工具已经执行")));
    assert.ok(logs.some((log) => log.includes("本次仅提醒")));
    assert.ok(logs.some((log) => log.includes("工具结果仍会进入模型上下文")));
    assert.ok(logs.some((log) => log.includes("已发生的外部副作用不会撤销")));
    assert.ok(!logs.some((log) => log.includes("password=[REDACTED]")));
    assert.ok(!logs.some((log) => log.includes("已被阻断")));
  });

  it("llm_output logs warning without raw evidence", async () => {
    const { hooks, logs } = registerHandlers(policyConfig("warn"));
    const llmOutput = hooks.find((hook) => hook.hookName === "llm_output");
    assert.ok(llmOutput);
    mockCli(scanResult("warn", [warnFinding]));

    const result = await llmOutput.handler(
      {
        assistantTexts: ["email alice@example.com"],
        sessionId: "session-1",
      },
      {},
    );

    assert.equal(result, undefined);
    assert.ok(logs.some((log) => log.includes("[WARN] [pii-checker] 检测到")));
    assert.ok(logs.some((log) => log.includes("检测到 1 项一般风险敏感信息")));
    assert.ok(logs.some((log) => log.includes("原始模型输出仍会交付")));
    assert.ok(logs.some((log) => log.includes("不会被脱敏或阻断")));
    assert.ok(!logs.some((log) => log.includes("a***@example.com")));
    assert.ok(!logs.some((log) => log.includes("alice@example.com")));
    assert.equal(lastCliArgs?.at(-1), "model_output");
  });

  it("CLI nonzero fails open", async () => {
    const { beforeDispatch } = registerHandlers(enableBlockConfig(true));
    mockCli({ exitCode: 1, stdout: "", stderr: "boom" });

    const result = await beforeDispatch.handler({ content: "email alice@example.com" });

    assert.equal(result, undefined);
  });

  it("invalid CLI JSON fails open", async () => {
    const { beforeDispatch, logs } = registerHandlers(enableBlockConfig(true));
    mockCli({ exitCode: 0, stdout: "not-json", stderr: "" });

    const result = await beforeDispatch.handler({ content: "email alice@example.com" });

    assert.equal(result, undefined);
    assert.ok(logs.some((log) => log.includes("CLI returned invalid JSON")));
  });

  it("short-circuits before config access when the environment switch is false", () => {
    process.env.PII_CHECKER_HOOK_ENABLED = "false";
    mockCliNoCall();
    const pluginConfig = new Proxy(
      {},
      {
        get() {
          throw new Error("plugin config should not be read when disabled");
        },
      },
    );
    const { api, hooks } = createMockApi(pluginConfig);

    piiScan.register(api);

    assert.deepEqual(hooks, []);
    assert.equal(lastCliArgs, undefined);
  });

  it("lets the environment switch override legacy piiScanUserInput=false", () => {
    process.env.PII_CHECKER_HOOK_ENABLED = "true";
    const { hooks } = registerHandlers({ piiScanUserInput: false });

    assert.equal(hooks.length, 4);
  });

  it("lets the environment policy override capability configuration", async () => {
    process.env.PII_CHECKER_MODE = "observe";
    const { beforeDispatch, logs } = registerHandlers(policyConfig("block"));
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result, undefined);
    assert.ok(!logs.some((log) => log.includes("[pii-checker] DENY")));
  });

  it("invalid environment mode falls back to observe", async () => {
    process.env.PII_CHECKER_MODE = "blcok";
    const { beforeDispatch, logs } = registerHandlers(policyConfig("block"));
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result, undefined);
    assert.ok(
      logs.some((log) =>
        log.includes("[WARN] [pii-checker] invalid PII_CHECKER_MODE; using observe"),
      ),
    );
    assert.ok(!logs.some((log) => log.includes("[pii-checker] DENY")));
  });

  it("maps deny in the environment mode to block", async () => {
    process.env.PII_CHECKER_MODE = "deny";
    const { beforeDispatch } = registerHandlers(policyConfig("observe"));
    mockCli(scanResult("deny", [denyFinding]));

    const result = await beforeDispatch.handler({ content: "password=secret" });

    assert.equal(result?.handled, true);
  });
});
