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
export const rawResult = "SYNTHETIC_RAW_SECRET";
export const reviewedResult = "REVIEWED_SAFE_BODY";
export const replacementOutput = {
  hookSpecificOutput: { hookEventName: "PostToolUse", updatedToolOutput: reviewedResult },
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
    options = [],
    postOutputs = [];
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
        for (const group of input.options.hooks.SessionStart ?? []) {
          for (const callback of group.hooks) {
            await callback({
              hook_event_name: "SessionStart",
              session_id: "session",
              source: "startup",
              cwd: input.options.cwd ?? root + "/work",
            });
          }
        }
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
        const failed = input.prompt === "failure" || input.prompt === "sensitive-failure";
        const postName = failed ? "PostToolUseFailure" : "PostToolUse";
        const post = {
          ...payload,
          hook_event_name: postName,
          ...(failed
            ? { error: input.prompt === "failure" ? "tool error" : rawResult, is_interrupt: false }
            : { tool_response: input.prompt === "sensitive" ? rawResult : "ok" }),
        };
        for (const group of input.options.hooks[postName]) {
          for (const callback of group.hooks) {
            const result = await callback(post, "call");
            postOutputs.push(result);
            if (result.continue === false) return;
          }
        }
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
    postOutputs,
    stopService,
    async configureReplacement() {
      const spec = configuration.spec;
      spec.providers.replacement = {
        ...spec.providers.policy,
        config: { record_file: root + "/post.log", fail_file: root + "/fail" },
        transport: {
          ...spec.providers.policy.transport,
          argv: ["/usr/bin/python3", new URL("../../../tests/fixtures/sdk-replace.py", import.meta.url).pathname],
        },
      };
      spec.events["tool.after"] = {
        enabled: true,
        required,
        steps: [
          { id: "observe", provider: "replacement", operation: "record", effects: ["observe"], on_error: "report" },
          { id: "replace", provider: "replacement", operation: "scan", effects: ["observe", "replace_result"], on_error: "report" },
        ],
      };
      await writeFile(configFile, JSON.stringify(configuration));
    },
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
    async descriptor() {
      const path = root + "/descriptor.json";
      const { stdout } = await execute(binary, [
        "config", "descriptor", "--config", configFile, "--agent", "app",
        "--profile", "qoder-public-hooks",
      ], { timeout: 8000 });
      await writeFile(path, stdout, { mode: 0o600 });
      return path;
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
