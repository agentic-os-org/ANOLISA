/** Hosted public-Hook denial and WarmQuery replacement using installed packages. */
import assert from "node:assert/strict";
import { mkdir, writeFile, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { once } from "node:events";
import { pathToFileURL } from "node:url";
const [binary, sdkFile, bridgeFile, coreFile, root, mode] = process.argv.slice(2);
const { startDaemon, startup, query } = await import(pathToFileURL(sdkFile));
const { Admission, Instance } = await import(pathToFileURL(coreFile));
const {
  createQoderBridge, qoderOptions, qoderCapabilities, qoderInstallRoutes,
} = await import(pathToFileURL(bridgeFile));
const rawResult = "SYNTHETIC_RAW_SECRET";
const reviewedResult = "REVIEWED_SAFE_BODY";
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
        scanner: {
          protocol: "aw-provider/v1alpha1",
          config: { record_file: root + "/post.log", fail_file: root + "/fail" },
          transport: {
            type: "stdio",
            location: "agent",
            argv: ["/usr/bin/python3", new URL("./fixtures/sdk-replace.py", import.meta.url).pathname],
          },
          timeout_ms: 2000,
          max_output_bytes: 4096,
        },
      },
      events: {
        ...(mode === "warm-post" ? { "tool.after": {
          enabled: true,
          required: true,
          steps: [{
            id: "observe", provider: "scanner", operation: "record", effects: ["observe"], on_error: "report",
          }, {
            id: "replace", provider: "scanner", operation: "scan", effects: ["observe", "replace_result"], on_error: "report",
          }],
        } } : {}),
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
async function startService() {
  assert(!existsSync(root + "/state/aw.sock"));
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
}
async function prepare() {
  modelBeforeAttach = true;
  await startService();
  await attach();
}
async function attach() {
  const response = await execute(
    binary,
    [
      "connect",
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
async function warmPost(fail) {
  const capabilities = qoderCapabilities(false);
  const instance = new Instance({
    endpoint: root + "/state/aw.sock",
    target: "app",
    availability: "required",
    cwd: root,
    environment: clean,
    capabilities,
    install: (routes) => qoderInstallRoutes(routes, false),
  });
  const admission = new Admission(instance, { events: ["tool.before", "tool.after"] });
  const fixture = root + "/synthetic.txt";
  await writeFile(fixture, rawResult + "\n");
  let nativeSawOriginal = false;
  let warm, session;
  const integration = qoderOptions(admission, {
    daemon, cwd: root, model: "auto", tools: ["Bash"], settingSources: [],
    permissionMode: "bypassPermissions", persistSession: false, maxTurns: 3,
    hooks: { PostToolUse: [{ matcher: "Bash", hooks: [async (payload) => {
      nativeSawOriginal ||= JSON.stringify(payload.tool_response).includes(rawResult);
      return {};
    }] }] },
  });
  try {
    instance.listen();
    warm = await startup({ options: integration.options });
    await attach();
    if (fail) await writeFile(root + "/fail", "");
    session = integration.bind(warm.query(
      `Use Bash exactly once to execute cat ${fixture}. Reply with the exact output of Bash. Do not retry.`,
    ));
    const messages = [];
    for await (const message of session) messages.push(message);
    const blocks = (kind, blockType) => messages.filter(({ type }) => type === kind)
      .flatMap(({ message }) => message.content).filter(({ type }) => type === blockType);
    const delivered = JSON.stringify(blocks("user", "tool_result"));
    const assistant = JSON.stringify(blocks("assistant", "text"));
    assert(nativeSawOriginal, "native PostToolUse Hook did not receive the original tool output");
    if (fail) {
      assert.equal(integration.failure?.code, "sdk_step_failed");
      return { original_in_tool_event: delivered.includes(rawResult), original_in_model_reply: assistant.includes(rawResult) };
    }
    assert.equal(integration.failure, undefined);
    assert(messages.some((msg) => msg.type === "result" && !msg.is_error));
    assert(delivered.includes(reviewedResult), "native tool event lacks reviewed output");
    assert(!delivered.includes(rawResult), "native tool event contains original output");
    assert(assistant.includes(reviewedResult), "model did not quote reviewed output");
    assert(!assistant.includes(rawResult), "model echoed original output");
    return true;
  } finally {
    admission.cancel();
    try {
      await session?.close();
    } finally {
      warm?.close();
      try { await admission.close(); } finally { await instance.close(); }
    }
  }
}

async function checkBridge() {
  aw = await createQoderBridge({
    configFile,
    availability: "required",
    executable: binary,
    query,
    cwd: root,
    attachTimeoutMs: 15000,
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
  return {
    model_before_attach: modelBeforeAttach,
    native_hooks_preserved: true,
    allowed_once: true,
    denied_side_effect_absent: true,
    native_before: nativeBefore,
    native_after: nativeAfter,
  };
}

try {
  daemon = await startDaemon({
    auth: {
      type: "accessToken",
      accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" },
    },
    stderr() {},
  });
  const sdk = JSON.parse(await readFile(new URL("../package.json", pathToFileURL(sdkFile)), "utf8"));
  let evidence;
  if (mode === "warm-post") {
    await startService();
    evidence = {
      warm_query_replacement: await warmPost(false),
      public_stop: await warmPost(true),
    };
  } else {
    evidence = await checkBridge();
  }
  await writeFile(root + "/result.json", JSON.stringify({
    status: "passed", sdk: sdk.version, worker: sdk.qoderCliVersion, ...evidence,
  }));
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
