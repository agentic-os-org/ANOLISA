import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { createQoderBridge } from "../src/index.mjs";

const execute = promisify(execFile);
const binary =
  process.env.AW_TEST_BINARY ??
  fileURLToPath(new URL("../../../../target/debug/aw", import.meta.url));
export const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
export const before = {
  hook_event_name: "PreToolUse",
  session_id: "session",
  tool_use_id: "call",
  tool_name: "Bash",
  tool_input: { command: "true" },
};
export async function fixture(required = true, input = false) {
  const root = await mkdtemp("/tmp/aw-sdk-bridge-");
  await mkdir(root + "/state", { mode: 0o700 });
  await mkdir(root + "/work");
  const configuration = {
    apiVersion: "aw/v1alpha1",
    kind: "AWConfiguration",
    metadata: { name: "bridge" },
    spec: {
      daemon: {
        startup: "external",
        endpoint: "auto",
        state_dir: root + "/state",
      },
      execution: { guarantee: "native_hook", default_event_budget_ms: 1000 },
      audit: { enabled: true, payload: "metadata_only" },
      agents: { app: { adapter: "qoder-sdk", argv: ["/bin/true"] } },
      providers: {
        policy: {
          protocol: "aw-provider/v1alpha1",
          config: { deny_file: root + "/deny" },
          transport: {
            type: "stdio",
            location: "agent",
            argv: [
              "/usr/bin/python3",
              fileURLToPath(
                new URL("../../../tests/fixtures/sdk-deny.py", import.meta.url),
              ),
            ],
          },
          timeout_ms: 1000,
          max_output_bytes: 4096,
        },
      },
      events: {
        "tool.before": {
          enabled: true,
          required,
          steps: [
            {
              id: "before",
              provider: "policy",
              operation: "check",
              effects: ["observe", "block"],
              on_error: "block",
            },
          ],
        },
        ...(input
          ? {
              "input.submit": {
                enabled: true,
                required,
                steps: [
                  {
                    id: "input",
                    provider: "policy",
                    operation: "check",
                    effects: ["observe", "block"],
                    on_error: "block",
                  },
                ],
              },
            }
          : {}),
      },
    },
  };
  const configFile = root + "/aw.json";
  await writeFile(configFile, JSON.stringify(configuration), { mode: 0o600 });
  let service, bridge;
  const stopService = async () => {
    if (service?.exitCode === null && service.signalCode === null) {
      const done = once(service, "exit");
      service.kill("SIGTERM");
      await done;
    }
    service = undefined;
  };
  const calls = [],
    options = [];
  const nativeQuery = (input) => {
    options.push(input.options);
    return {
      closed: false,
      async close() {
        this.closed = true;
      },
      async interrupt() {
        calls.push("interrupt");
      },
      async initializationResult() {
        return "native-ready";
      },
      async *[Symbol.asyncIterator]() {
        if (input.options.hooks.UserPromptSubmit) {
          calls.push("input");
          for (const group of input.options.hooks.UserPromptSubmit) {
            const result = await group.hooks[0]({
              hook_event_name: "UserPromptSubmit",
              session_id: "session",
              cwd: input.options.cwd ?? root + "/work",
              prompt: input.prompt,
            });
            if (result.decision === "block") {
              calls.push("input-denied");
              return;
            }
          }
        }
        calls.push("model");
        yield { type: "model" };
        if (input.prompt === "text") return;
        const payload = { ...before, cwd: input.options.cwd ?? root + "/work" };
        for (const group of input.options.hooks.PreToolUse) {
          const result = await group.hooks[0](payload, "call");
          if (result.hookSpecificOutput?.permissionDecision === "deny") {
            calls.push("denied");
            return;
          }
        }
        calls.push("tool");
        for (const group of input.options.hooks.PostToolUse)
          await group.hooks[0](
            {
              ...payload,
              hook_event_name: "PostToolUse",
              tool_response: "ok",
            },
            "call",
          );
        yield { type: "result" };
      },
    };
  };
  return {
    root,
    configFile,
    configuration,
    calls,
    options,
    stopService,
    async bridge(extra = {}) {
      bridge = await createQoderBridge({
        configFile,
        executable: binary,
        query: nativeQuery,
        availability: required ? "required" : "native_fallback",
        cwd: root + "/work",
        attachTimeoutMs: 300,
        ...extra,
      });
      return bridge;
    },
    async attach() {
      service = spawn(
        binary,
        [
          "service",
          "serve",
          "--config",
          configFile,
          "--state-dir",
          root + "/state",
        ],
        { stdio: "ignore" },
      );
      for (let i = 0; i < 200 && !existsSync(root + "/state/aw.sock"); i++)
        await delay(10);
      const response = await execute(
        binary,
        [
          "connect",
          "--socket",
          root + "/state/aw.sock",
          "--sdk-pid",
          String(process.pid),
          "--timeout-ms",
          "5000",
        ],
        { timeout: 8000 },
      );
      assert.equal(JSON.parse(response.stdout).result.state, "completed");
    },
    async close() {
      try {
        await bridge?.close();
      } finally {
        await stopService();
        await rm(root, { recursive: true, force: true });
      }
    },
  };
}
