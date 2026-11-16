/** Hosted native adoption: prompt precedes AW, native Hooks coexist, denial prevents effects. */
import assert from "node:assert/strict";
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { once } from "node:events";
import { pathToFileURL } from "node:url";
const [binary, sdkFile, bridgeFile, root] = process.argv.slice(2);
const { startDaemon, query } = await import(pathToFileURL(sdkFile));
const { createQoderBridge } = await import(pathToFileURL(bridgeFile));
const execute = promisify(execFile);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const clean = { ...process.env };
delete clean.QODER_PERSONAL_ACCESS_TOKEN;
let daemon,
  aw,
  service,
  preparing,
  modelBeforeAttach = false,
  nativeBefore = 0,
  nativeAfter = 0;
await mkdir(root + "/state", { mode: 0o700, recursive: true });
const configFile = root + "/aw.json";
await writeFile(
  configFile,
  JSON.stringify({
    apiVersion: "aw/v1alpha1",
    kind: "AWConfiguration",
    metadata: { name: "native-bridge" },
    spec: {
      daemon: {
        startup: "external",
        endpoint: "auto",
        state_dir: root + "/state",
      },
      execution: { guarantee: "native_hook", default_event_budget_ms: 5000 },
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
              new URL("./fixtures/sdk-deny.py", import.meta.url).pathname,
            ],
          },
          timeout_ms: 2000,
          max_output_bytes: 4096,
        },
      },
      events: {
        "tool.before": {
          enabled: true,
          required: true,
          steps: [
            {
              id: "check",
              provider: "policy",
              operation: "check",
              effects: ["observe", "block"],
              on_error: "block",
            },
          ],
        },
      },
    },
  }),
  { mode: 0o600 },
);
async function prepare() {
  assert(!existsSync(root + "/state/aw.sock"));
  modelBeforeAttach = true;
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
    { env: clean, stdio: "ignore" },
  );
  for (let i = 0; i < 200 && !existsSync(root + "/state/aw.sock"); i++)
    await sleep(10);
  const response = await execute(
    binary,
    [
      "instance",
      "attach",
      "--socket",
      root + "/state/aw.sock",
      "--sdk-pid",
      String(process.pid),
      "--timeout-ms",
      "10000",
    ],
    { env: clean, timeout: 15000 },
  );
  assert.equal(JSON.parse(response.stdout).result.state, "completed");
}
try {
  aw = await createQoderBridge({
    configFile,
    availability: "required",
    executable: binary,
    query,
    cwd: root,
    attachTimeoutMs: 15000,
  });
  daemon = await startDaemon({
    auth: {
      type: "accessToken",
      accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" },
    },
    stderr() {},
  });
  const hooks = {
    PreToolUse: [
      {
        matcher: "Bash",
        hooks: [
          async () => {
            nativeBefore++;
            preparing ??= prepare();
            preparing.catch(() => {});
            return {};
          },
        ],
      },
    ],
    PostToolUse: [
      {
        matcher: "Bash",
        hooks: [
          async () => {
            nativeAfter++;
            return {};
          },
        ],
      },
    ],
  };
  for (const name of ["allowed", "denied"]) {
    if (name === "denied") await writeFile(root + "/deny", "deny");
    const session = aw.query({
      prompt: `Use Bash exactly once to execute: printf 'ONE\\n' >> ${root}/${name}.txt . Then reply DONE. Do not retry a denied tool.`,
      options: {
        daemon,
        cwd: root,
        model: "auto",
        tools: ["Bash"],
        settingSources: [],
        permissionMode: "bypassPermissions",
        persistSession: false,
        maxTurns: 3,
        hooks,
      },
    });
    let result;
    for await (const message of session)
      if (message.type === "result") result = message;
    assert(result && !result.is_error, "hosted Query must complete");
    await preparing;
  }
  assert.equal(await readFile(root + "/allowed.txt", "utf8"), "ONE\n");
  assert(!existsSync(root + "/denied.txt"));
  assert(modelBeforeAttach && nativeBefore >= 2 && nativeAfter >= 1);
  await writeFile(
    root + "/result.json",
    JSON.stringify({
      status: "passed",
      model_before_attach: modelBeforeAttach,
      native_hooks_preserved: true,
      allowed_once: true,
      denied_side_effect_absent: true,
      native_before: nativeBefore,
      native_after: nativeAfter,
    }),
  );
} finally {
  try {
    await aw?.close();
  } finally {
    await daemon?.stop();
    if (service?.exitCode === null) {
      const done = once(service, "exit");
      service.kill("SIGTERM");
      await done;
    }
  }
}
