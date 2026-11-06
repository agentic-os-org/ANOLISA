// tests/unit/code-scan-native-self-protect-test.ts
//
// Self-protect must also cover OpenClaw's native plugin admin tools: they carry
// no shell text, so scan-code never sees them and the plugin would be disabled.
import { describe, it, beforeEach, afterEach } from "node:test";
import assert from "node:assert/strict";
import { codeScan } from "../../src/capabilities/code-scan.js";
import { _setCliMock, _resetCliMock } from "../../src/utils.js";

type RegisteredHook = {
  hookName: string;
  handler: (event: any, ctx: any) => Promise<any>;
  priority: number;
};

function createMockApi() {
  const hooks: RegisteredHook[] = [];
  const logs: string[] = [];
  const api = {
    pluginConfig: {},
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

function handler() {
  const { api, hooks, logs } = createMockApi();
  codeScan.register(api);
  assert.equal(hooks.length, 1, "scan-code should register exactly 1 hook");
  return { handler: hooks[0].handler, logs };
}

let cliCalls = 0;
let previousHookEnabled: string | undefined;

function spyCli() {
  cliCalls = 0;
  _setCliMock(async () => {
    cliCalls += 1;
    return { exitCode: 0, stdout: JSON.stringify({ verdict: "pass", findings: [] }), stderr: "" };
  });
}

const ctx = { sessionId: "s", runId: "r", toolCallId: "t" };

describe("scan-code self-protect on native plugin tools", () => {
  beforeEach(() => {
    previousHookEnabled = process.env.CODE_SCANNER_HOOK_ENABLED;
    delete process.env.CODE_SCANNER_HOOK_ENABLED;
    spyCli();
  });

  afterEach(() => {
    _resetCliMock();
    if (previousHookEnabled === undefined) {
      delete process.env.CODE_SCANNER_HOOK_ENABLED;
    } else {
      process.env.CODE_SCANNER_HOOK_ENABLED = previousHookEnabled;
    }
  });

  for (const [label, event] of [
    [
      "plugins tool with pluginId",
      { toolName: "plugins", params: { action: "disable", pluginId: "agent-sec" } },
    ],
    [
      "plugins tool with name",
      { toolName: "plugins", params: { action: "disable", name: "agent-sec" } },
    ],
    [
      "uninstall through the plugins tool",
      { toolName: "plugins", params: { action: "uninstall", pluginId: "agent-sec" } },
    ],
    [
      "tool_call wrapper with nested args",
      { toolName: "tool_call", params: { id: "plugins", args: { action: "disable", pluginId: "agent-sec" } } },
    ],
    [
      "sec-core alias",
      { toolName: "plugins", params: { action: "disable", pluginId: "agent-sec-core" } },
    ],
  ] as const) {
    it(`blocks ${label}`, async () => {
      const { handler: onToolCall, logs } = handler();

      const result = await onToolCall(event, ctx);

      assert.equal(result?.block, true, `${label} must be blocked`);
      assert.match(String(result?.blockReason), /self|自我保护/i);
      assert.match(logs.join("\n"), /SELF-PROTECT block/);
      assert.match(logs.join("\n"), /self-protect-native/);
      assert.equal(cliCalls, 0, "native mutations are not shell text to scan");
    });
  }

  for (const [label, event] of [
    [
      "disabling a third-party plugin",
      { toolName: "plugins", params: { action: "disable", pluginId: "other-plugin" } },
    ],
    [
      "listing plugins",
      { toolName: "plugins", params: { action: "list" } },
    ],
    [
      "enabling agent-sec",
      { toolName: "plugins", params: { action: "enable", pluginId: "agent-sec" } },
    ],
  ] as const) {
    it(`allows ${label}`, async () => {
      const { handler: onToolCall } = handler();

      const result = await onToolCall(event, ctx);

      assert.equal(result, undefined, `${label} is not a self-protect case`);
      assert.equal(cliCalls, 0);
    });
  }

  it("keeps scanning exec commands", async () => {
    const { handler: onToolCall } = handler();

    const result = await onToolCall({ toolName: "exec", params: { command: "ls -la" } }, ctx);

    assert.equal(result, undefined);
    assert.equal(cliCalls, 1, "the shell path still goes through scan-code");
  });
});

describe("self-protect target matching", () => {
  beforeEach(() => {
    previousHookEnabled = process.env.CODE_SCANNER_HOOK_ENABLED;
    delete process.env.CODE_SCANNER_HOOK_ENABLED;
    spyCli();
  });

  afterEach(() => {
    _resetCliMock();
    if (previousHookEnabled === undefined) {
      delete process.env.CODE_SCANNER_HOOK_ENABLED;
    } else {
      process.env.CODE_SCANNER_HOOK_ENABLED = previousHookEnabled;
    }
  });

  for (const [label, target] of [
    ["a versioned id", "agent-sec@1.1.2"],
    ["a resolved entry path", "global:agent-sec/dist/index.js"],
    ["the security product name", "agent-security"],
  ] as const) {
    it(`blocks ${label}`, async () => {
      const { handler: onToolCall } = handler();

      const result = await onToolCall(
        { toolName: "plugins", params: { action: "disable", pluginId: target } },
        ctx,
      );

      assert.equal(result?.block, true);
    });
  }

  it("allows a plugin whose id only starts like agent-sec", async () => {
    const { handler: onToolCall } = handler();

    const result = await onToolCall(
      { toolName: "plugins", params: { action: "disable", pluginId: "agent-sec-community" } },
      ctx,
    );

    assert.equal(result, undefined);
  });
});
