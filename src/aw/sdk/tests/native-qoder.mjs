import assert from "node:assert/strict";
import { readFile, writeFile, mkdtemp, mkdir, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { once } from "node:events";
import { Instance } from "@anolisa/aw-sdk";
import { qoderOptions } from "@anolisa/aw-sdk/qoder";
import { query, startDaemon } from "@qoder-ai/qoder-agent-sdk";
const binary = process.env.AW_TEST_BINARY;
assert(binary, "AW_TEST_BINARY is required");
assert(
  process.env.QODER_PERSONAL_ACCESS_TOKEN,
  "Qoder authentication is required",
);
const root = await mkdtemp("/tmp/aw-sdk-native-");
const state = root + "/state";
await mkdir(state, { mode: 0o700 });
const work = root + "/work";
await mkdir(work, { mode: 0o700 });
const config = {
  apiVersion: "aw/v1alpha1",
  kind: "AWConfiguration",
  metadata: { name: "sdk-native" },
  spec: {
    daemon: { startup: "external", endpoint: "auto", state_dir: state },
    execution: { guarantee: "native_hook", default_event_budget_ms: 5000 },
    audit: { enabled: true, payload: "metadata_only" },
    agents: { app: { adapter: "qoder-sdk", argv: ["/bin/true"] } },
    providers: {},
    events: Object.fromEntries(
      ["tool.before", "tool.after"].map((name) => [
        name,
        { enabled: true, required: false, steps: [] },
      ]),
    ),
  },
};
config.spec.providers.policy = {
  protocol: "aw-provider/v1alpha1",
  config: { deny_file: root + "/deny" },
  transport: {
    type: "stdio",
    location: "agent",
    argv: [
      "/usr/bin/python3",
      new URL("./sdk-deny.py", import.meta.url).pathname,
    ],
  },
  timeout_ms: 2000,
  max_output_bytes: 4096,
};
config.spec.events["tool.before"].steps = [
  {
    id: "before",
    provider: "policy",
    operation: "check",
    effects: ["observe", "block"],
    on_error: "block",
  },
];
await writeFile(root + "/config.json", JSON.stringify(config));
let service, daemon, active, task;
async function startService() {
  service = spawn(
    binary,
    [
      "service",
      "serve",
      "--config",
      root + "/config.json",
      "--state-dir",
      state,
    ],
    { stdio: "ignore" },
  );
  for (let i = 0; i < 100 && !existsSync(state + "/aw.sock"); i++)
    await new Promise((r) => setTimeout(r, 20));
  assert(existsSync(state + "/aw.sock"));
}
async function stopService() {
  if (service?.exitCode === null) {
    const stopped = once(service, "exit");
    service.kill("SIGTERM");
    await stopped;
  }
}
const client = new Instance({
  endpoint: state + "/aw.sock",
  target: "app",
  cwd: work,
  availability: "native_fallback",
  timeoutMs: 1000,
  capabilities: {
    adapter: "qoder-sdk",
    version: "public-hooks/v1",
    entrypoint: "sdk-worker",
    events: { "tool.before": ["observe", "block"], "tool.after": ["observe"] },
  },
  install: () => true,
});
const execute = promisify(execFile);
async function until(check) {
  const deadline = Date.now() + 8000;
  while (!check()) {
    assert(Date.now() < deadline, "SDK control readiness");
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
async function control(action) {
  const { stdout } = await execute(
    binary,
    [
      action,
      "--socket",
      state + "/aw.sock",
      "--sdk-instance",
      client.applicationId,
    ],
    { timeout: 10000 },
  );
  assert.equal(JSON.parse(stdout).result.state, "completed");
}
const results = [];
try {
  daemon = await startDaemon({
    auth: {
      type: "accessToken",
      accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" },
    },
    workerIdleTimeoutMs: 300000,
    debugFile: root + "/private.log",
    stderr: () => {},
  });
  client.listen();
  await startService();
  await until(() => client.applicationId);
  await control("connect");
  await until(() => client.state === "ready");
  for (const mode of [
    "online",
    "detached",
    "attached",
    "loss",
    "recovered",
    "aw-deny",
    "native-deny",
  ]) {
    if (mode === "recovered") {
      await startService();
      await until(() => client.state === "ready");
    }
    if (mode === "detached") {
      await control("disconnect");
      await until(() => client.state === "detached");
      await writeFile(root + "/deny", "1");
    }
    if (mode === "attached") {
      await rm(root + "/deny");
      await control("connect");
      await until(() => client.state === "ready");
    }
    if (mode === "aw-deny") await writeFile(root + "/deny", "1");
    if (mode === "native-deny") await rm(root + "/deny");
    task = await client.task();
    const calls = [];
    const hooks = {
      PreToolUse: [
        {
          matcher: "*",
          hooks: [
            async (input) => {
              calls.push("native-before");
              if (mode === "loss") await stopService();
              return mode === "native-deny"
                ? {
                    hookSpecificOutput: {
                      hookEventName: "PreToolUse",
                      permissionDecision: "deny",
                      permissionDecisionReason:
                        "Native policy denied. Stop without retry.",
                    },
                  }
                : {};
            },
          ],
        },
      ],
      PostToolUse: [
        {
          matcher: "*",
          hooks: [
            async () => {
              calls.push("native-after");
              return {};
            },
          ],
        },
      ],
    };
    const attached = qoderOptions(task, {
      daemon,
      cwd: work,
      settingSources: [],
      persistSession: false,
      permissionMode: "bypassPermissions",
      tools: ["Bash"],
      model: "auto",
      maxTurns: 2,
      hooks,
    });
    const original = task.invoke.bind(task);
    task.invoke = async (...args) => {
      calls.push("aw-" + args[0].event);
      return original(...args);
    };
    active = attached.bind(
      query({
        prompt: `Use Bash exactly once to run "printf x >> ${mode}.txt". Then stop. Do not retry.`,
        options: attached.options,
      }),
    );
    let result;
    for await (const message of active) {
      if (message.type === "result") result = message;
    }
    assert(result);
    assert.equal(
      result.is_error,
      false,
      JSON.stringify({ subtype: result.subtype }),
    );
    if (mode.endsWith("deny"))
      assert(!existsSync(work + "/" + mode + ".txt"), "denied side effect");
    else
      assert.equal(
        await readFile(work + "/" + mode + ".txt", "utf8"),
        "x",
        "exactly once effect",
      );
    assert(calls.includes("native-before"));
    if (!mode.endsWith("deny")) assert(calls.includes("native-after"));
    assert(!attached.failure);
    assert.equal(task.fallback, ["loss", "detached"].includes(mode));
    if (!["loss", "detached", "aw-deny", "native-deny"].includes(mode))
      assert(calls.includes("aw-tool.after"));
    results.push({
      mode,
      passed: true,
      calls,
      fallback: task.fallback,
      subtype: result.subtype,
    });
    await active.close();
    active = null;
    await task.close();
    task = null;
  }
  console.log(JSON.stringify({ passed: true, cases: results }));
} finally {
  await active?.close();
  await task?.close();
  await client.close();
  await stopService();
  await daemon?.stop();
  await rm(root, { recursive: true, force: true });
}
