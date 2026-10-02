// tests/unit/code-scan-test.ts
import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { codeScan } from "../../src/capabilities/code-scan.js";
import { _setCliMock, _resetCliMock } from "../../src/utils.js";
import type { CliResult } from "../../src/utils.js";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

type RegisteredHook = {
  hookName: string;
  handler: (event: any, ctx: any) => Promise<any>;
  priority: number;
};

/** Create a minimal mock OpenClaw API and capture hook registrations. */
function createMockApi(pluginConfig: Record<string, any> = {}) {
  const hooks: RegisteredHook[] = [];
  const logs: string[] = [];
  const api = {
    pluginConfig,
    logger: {
      info: (msg: string) => logs.push(msg),
      error: (msg: string) => logs.push(msg),
      warn: (msg: string) => logs.push(msg),
      debug: (msg: string) => logs.push(msg),
    },
    on: (hookName: string, handler: any, opts?: { priority?: number }) => {
      hooks.push({ hookName, handler, priority: opts?.priority ?? 0 });
    },
  };
  return { api: api as any, hooks, logs };
}

/** Register scan-code and return the single captured handler. */
function registerAndGetHandler(pluginConfig: Record<string, any> = {}) {
  const { api, hooks, logs } = createMockApi(pluginConfig);
  codeScan.register(api);
  assert.equal(hooks.length, 1, "scan-code should register exactly 1 hook");
  return { handler: hooks[0].handler, hooks, logs };
}

/** Standard exec event factory. */
function execEvent(command: string) {
  return { toolName: "exec", params: { command } };
}

/** Captured CLI args from last mock call. */
let lastCliArgs: string[] | undefined;
let lastCliOpts: { timeout?: number } | undefined;
let previousCodeScannerHookEnabled: string | undefined;
let previousCodeScannerMode: string | undefined;

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("scan-code", () => {
  beforeEach(() => {
    lastCliArgs = undefined;
    lastCliOpts = undefined;
    previousCodeScannerHookEnabled = process.env.CODE_SCANNER_HOOK_ENABLED;
    previousCodeScannerMode = process.env.CODE_SCANNER_MODE;
    delete process.env.CODE_SCANNER_HOOK_ENABLED;
    delete process.env.CODE_SCANNER_MODE;
  });

  afterEach(() => {
    _resetCliMock();
    if (previousCodeScannerHookEnabled === undefined) {
      delete process.env.CODE_SCANNER_HOOK_ENABLED;
    } else {
      process.env.CODE_SCANNER_HOOK_ENABLED = previousCodeScannerHookEnabled;
    }
    if (previousCodeScannerMode === undefined) {
      delete process.env.CODE_SCANNER_MODE;
    } else {
      process.env.CODE_SCANNER_MODE = previousCodeScannerMode;
    }
  });

  // =========================================================================
  // Dimension 2: Hook Registration Correctness
  // =========================================================================
  describe("hook registration", () => {
    it("registers exactly 1 handler on before_tool_call", () => {
      const { hooks } = registerAndGetHandler();
      assert.equal(hooks[0].hookName, "before_tool_call");
    });

    it("cap.hooks array matches registered hook name", () => {
      const { hooks } = registerAndGetHandler();
      assert.deepEqual(codeScan.hooks, [hooks[0].hookName]);
    });

    it("uses default priority (0)", () => {
      const { hooks } = registerAndGetHandler();
      assert.equal(hooks[0].priority, 0);
    });
  });

  // =========================================================================
  // Dimension 3a: Event-to-CLI Parameter Transformation
  // =========================================================================
  describe("event → CLI args transformation", () => {
    it("CODE_SCANNER_HOOK_ENABLED=false → no CLI call", async () => {
      process.env.CODE_SCANNER_HOOK_ENABLED = "false";
      const { handler } = registerAndGetHandler();
      mockCliNoCall();

      const result = await handler(execEvent("rm -rf /"), {});

      assert.equal(result, undefined);
      assert.equal(lastCliArgs, undefined);
    });

    it("invalid CODE_SCANNER_HOOK_ENABLED value → defaults enabled silently", async () => {
      process.env.CODE_SCANNER_HOOK_ENABLED = "maybe";
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 0, stdout: '{"verdict":"pass","findings":[]}', stderr: "" });

      const result = await handler(execEvent("echo hello"), {});

      assert.equal(result, undefined);
      assert.deepEqual(lastCliArgs?.slice(2), ["scan-code", "--code", "echo hello", "--language", "bash"]);
    });

    it("exec tool with command → correct CLI args", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 0, stdout: '{"verdict":"pass","findings":[]}', stderr: "" });

      await handler(execEvent("rm -rf /"), {});

      assert.deepEqual(lastCliArgs?.slice(0, 2), [
        "--trace-context",
        JSON.stringify({ agent_name: "openclaw" }),
      ]);
      assert.deepEqual(lastCliArgs?.slice(2), ["scan-code", "--code", "rm -rf /", "--language", "bash"]);
      assert.equal(lastCliOpts?.timeout, 10000);
    });

    it("exec tool with trace context → injects trace context before scan-code", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 0, stdout: '{"verdict":"pass","findings":[]}', stderr: "" });

      await handler(
        {
          ...execEvent("pwd"),
          sessionId: "session-1",
          runId: "run-1",
          toolCallId: "tool-1",
          trace: { traceId: "nested-trace-is-not-hook-input" },
        },
        {},
      );

      assert.deepEqual(lastCliArgs?.slice(0, 2), [
        "--trace-context",
        JSON.stringify({
          agent_name: "openclaw",
          session_id: "session-1",
          run_id: "run-1",
          tool_call_id: "tool-1",
        }),
      ]);
      assert.deepEqual(lastCliArgs?.slice(2), ["scan-code", "--code", "pwd", "--language", "bash"]);
    });

    it("non-exec tool (read_file) → no CLI call", async () => {
      const { handler } = registerAndGetHandler();
      mockCliNoCall();

      const result = await handler({ toolName: "read_file", params: {} }, {});
      assert.equal(result, undefined);
    });

    it("non-exec tool (search) → no CLI call", async () => {
      const { handler } = registerAndGetHandler();
      mockCliNoCall();

      const result = await handler({ toolName: "search", params: { query: "x" } }, {});
      assert.equal(result, undefined);
    });

    it("exec with empty command → no CLI call", async () => {
      const { handler } = registerAndGetHandler();
      mockCliNoCall();

      const result = await handler(execEvent(""), {});
      assert.equal(result, undefined);
    });

    it("exec with non-string command → no CLI call", async () => {
      const { handler } = registerAndGetHandler();
      mockCliNoCall();

      const result = await handler({ toolName: "exec", params: { command: 123 } }, {});
      assert.equal(result, undefined);
    });

    it("exec with missing command param → no CLI call", async () => {
      const { handler } = registerAndGetHandler();
      mockCliNoCall();

      const result = await handler({ toolName: "exec", params: {} }, {});
      assert.equal(result, undefined);
    });

    it("command with special chars → passed verbatim to CLI", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 0, stdout: '{"verdict":"pass","findings":[]}', stderr: "" });

      await handler(execEvent('echo "hello world"'), {});

      assert.deepEqual(lastCliArgs?.slice(0, 2), [
        "--trace-context",
        JSON.stringify({ agent_name: "openclaw" }),
      ]);
      assert.deepEqual(lastCliArgs?.slice(2), ["scan-code", "--code", 'echo "hello world"', "--language", "bash"]);
    });
  });

  // =========================================================================
  // Dimension 3b: CLI Output-to-Hook Action Mapping
  // =========================================================================
  describe("CLI output → hook action mapping", () => {
    it("pass verdict → undefined (allow)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 0, stdout: '{"verdict":"pass","findings":[]}', stderr: "" });

      const result = await handler(execEvent("ls"), {});
      assert.equal(result, undefined);
    });

    it("deny with 1 finding, default config → undefined (log only)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny","findings":[{"desc_zh":"危险命令"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("rm -rf /"), {});
      assert.equal(result, undefined);
    });

    it("deny with 1 finding, codeScanRequireApproval=true → { requireApproval }", async () => {
      const { handler } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny","findings":[{"desc_zh":"危险命令"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("rm -rf /"), {});

      assert.ok(result.requireApproval);
      assert.equal(result.requireApproval.title, "Code Scanner Security Warning");
      assert.equal(result.requireApproval.severity, "warning");
      assert.ok(result.requireApproval.description.includes("[code-scanner] Detected 1 issue(s):"));
      assert.ok(result.requireApproval.description.includes("- 危险命令"));
      assert.ok(result.requireApproval.description.includes("Command: rm -rf /"));
    });

    it("deny with 2 findings, codeScanRequireApproval=true → requireApproval.description contains both", async () => {
      const { handler } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny","findings":[{"desc_zh":"A"},{"desc_zh":"B"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("bad-cmd"), {});

      assert.ok(result.requireApproval);
      assert.ok(result.requireApproval.description.includes("Detected 2 issue(s):"));
      assert.ok(result.requireApproval.description.includes("- A"));
      assert.ok(result.requireApproval.description.includes("- B"));
    });

    it("warn with findings, default config → undefined (log only)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"warn","findings":[{"desc_zh":"注意"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});
      assert.equal(result, undefined);
    });

    it("warn with findings, codeScanRequireApproval=true → { requireApproval }", async () => {
      const { handler } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"warn","findings":[{"desc_zh":"注意"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.ok(result.requireApproval);
      assert.equal(result.requireApproval.title, "Code Scanner Security Warning");
      assert.equal(result.requireApproval.severity, "warning");
      assert.ok(result.requireApproval.description.includes("- 注意"));
    });

    it("CODE_SCANNER_MODE=ask overrides disabled approval config", async () => {
      process.env.CODE_SCANNER_MODE = "ask";
      const { handler } = registerAndGetHandler({ codeScanRequireApproval: false });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"warn","findings":[{"desc_zh":"注意"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.ok(result.requireApproval);
      assert.equal(result.block, undefined);
    });

    it("CODE_SCANNER_MODE=observe overrides enabled approval config", async () => {
      process.env.CODE_SCANNER_MODE = "observe";
      const { handler } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny","findings":[{"desc_zh":"危险"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.equal(result, undefined);
    });

    it("CODE_SCANNER_MODE=block overrides approval config and blocks warn findings", async () => {
      process.env.CODE_SCANNER_MODE = "block";
      const { handler, logs } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"warn","findings":[{"desc_zh":"注意"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.equal(result.block, true);
      assert.equal(result.requireApproval, undefined);
      assert.ok(result.blockReason.includes("[code-scanner] Detected 1 issue(s):"));
      assert.ok(result.blockReason.includes("- 注意"));
      assert.equal(logs.some((log) => log.includes("CODE_SCANNER_MODE")), false);
    });

    it("CODE_SCANNER_MODE=deny aliases block and blocks deny findings", async () => {
      process.env.CODE_SCANNER_MODE = "deny";
      const { handler, logs } = registerAndGetHandler({ codeScanRequireApproval: false });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny","findings":[{"desc_zh":"危险"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.equal(result.block, true);
      assert.equal(result.requireApproval, undefined);
      assert.ok(result.blockReason.includes("- 危险"));
      assert.equal(logs.some((log) => log.includes("CODE_SCANNER_MODE")), false);
    });

    it("debug alias selects the existing observe interaction", async () => {
      process.env.CODE_SCANNER_MODE = "debug";
      const { handler, logs } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"warn","findings":[{"desc_zh":"注意"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.equal(result, undefined);
      assert.equal(logs.some((log) => log.includes("CODE_SCANNER_MODE")), false);
    });

    it("invalid MODE is equivalent to an unset MODE", async () => {
      process.env.CODE_SCANNER_MODE = "invalid";
      const { handler, logs } = registerAndGetHandler({ codeScanRequireApproval: true });
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"warn","findings":[{"desc_zh":"注意"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("risky-cmd"), {});

      assert.ok(result.requireApproval);
      assert.equal(result.block, undefined);
      assert.ok(logs.some((log) => log.includes("CODE_SCANNER_MODE") && log.includes("invalid")));
    });

    it("deny but empty findings → undefined (findings.length === 0 gate)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny","findings":[]}',
        stderr: "",
      });

      const result = await handler(execEvent("cmd"), {});
      assert.equal(result, undefined);
    });

    it("unknown verdict with findings → undefined (falls through)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"info","findings":[{"desc_zh":"x"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("cmd"), {});
      assert.equal(result, undefined);
    });

    it("missing verdict field → undefined (no branch matches)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: '{"findings":[{"desc_zh":"x"}]}',
        stderr: "",
      });

      const result = await handler(execEvent("cmd"), {});
      assert.equal(result, undefined);
    });

    it("missing findings field → undefined (defaults to [], length === 0)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: '{"verdict":"deny"}',
        stderr: "",
      });

      const result = await handler(execEvent("cmd"), {});
      assert.equal(result, undefined);
    });
  });

  // =========================================================================
  // Dimension 4: Fail-Open Guarantee
  // =========================================================================
  describe("fail-open guarantee", () => {
    it("CLI failure (exitCode 1) → undefined, never throws", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 1, stdout: "", stderr: "command not found" });

      const result = await handler(execEvent("ls"), {});
      assert.equal(result, undefined);
    });

    it("CLI timeout (exitCode 124) → undefined, never throws", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 124, stdout: "", stderr: "timed out" });

      const result = await handler(execEvent("ls"), {});
      assert.equal(result, undefined);
    });

    it("malformed JSON → undefined (JSON.parse caught)", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({ exitCode: 0, stdout: "not json at all", stderr: "" });

      const result = await handler(execEvent("ls"), {});
      assert.equal(result, undefined);
    });

    it("CLI mock throws → undefined (catch block)", async () => {
      const { handler } = registerAndGetHandler();
      _setCliMock(async () => { throw new Error("process crashed"); });

      const result = await handler(execEvent("ls"), {});
      assert.equal(result, undefined);
    });
  });

  // =========================================================================
  // Dimension 5: Self-Protect Forced Block
  // =========================================================================
  describe("self-protect forced block", () => {
    it("self-protect-openclaw finding forces block regardless of config", async () => {
      const { handler } = registerAndGetHandler(); // default: codeScanRequireApproval=false
      mockCli({
        exitCode: 0,
        stdout: JSON.stringify({
          verdict: "warn",
          findings: [{ rule_id: "shell-self-protect-openclaw", desc_zh: "禁用 agent-sec 插件" }],
        }),
        stderr: "",
      });

      const result = await handler(
        execEvent("openclaw config set plugins.entries.agent-sec.enabled false"),
        {},
      );

      assert.ok(result);
      assert.equal(result.block, true);
      assert.ok(result.blockReason.includes("自我保护"));
      assert.ok(result.blockReason.includes("手动执行"));
    });

    it("self-protect block includes the original command in message", async () => {
      const { handler } = registerAndGetHandler();
      const cmd = "openclaw config set plugins.entries.agent-sec.enabled false && openclaw gateway restart";
      mockCli({
        exitCode: 0,
        stdout: JSON.stringify({
          verdict: "warn",
          findings: [{ rule_id: "shell-self-protect-openclaw", desc_zh: "禁用 agent-sec 插件" }],
        }),
        stderr: "",
      });

      const result = await handler(execEvent(cmd), {});

      assert.ok(result);
      assert.equal(result.block, true);
      assert.ok(result.blockReason.includes(cmd));
    });

    it("non-self-protect deny finding does not force block without codeScanRequireApproval", async () => {
      const { handler } = registerAndGetHandler(); // codeScanRequireApproval=false
      mockCli({
        exitCode: 0,
        stdout: JSON.stringify({
          verdict: "deny",
          findings: [{ rule_id: "shell-recursive-delete", desc_zh: "危险删除" }],
        }),
        stderr: "",
      });

      const result = await handler(execEvent("rm -rf /"), {});
      assert.equal(result, undefined);
    });

    it("self-protect hermes finding does NOT trigger block in openclaw hook", async () => {
      const { handler } = registerAndGetHandler();
      mockCli({
        exitCode: 0,
        stdout: JSON.stringify({
          verdict: "warn",
          findings: [{ rule_id: "shell-self-protect-hermes", desc_zh: "禁用 hermes 插件" }],
        }),
        stderr: "",
      });

      const result = await handler(
        execEvent("hermes plugins disable agent-sec-core-hermes-plugin"),
        {},
      );
      // hermes rule should not trigger openclaw self-protect
      assert.equal(result, undefined);
    });
  });
});
