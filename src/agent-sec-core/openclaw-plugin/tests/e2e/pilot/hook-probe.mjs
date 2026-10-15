import fs from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

import { normalizeJsonValue, sleep, withTimeout, writeExecutable } from "./common.mjs";
import { serializeError } from "./errors.mjs";

const DEFAULT_HOOK_TIMEOUT_MS = 35_000;

export async function runHookProbe({
  env,
  logsDir,
  openclawBin,
  openclawVersion,
  pluginRoot,
  repoRoot,
  workdir,
  skipFailureProbes,
}) {
  // Hook probe is supplemental coverage. It exercises direct plugin API handlers
  // and fail-open behavior that are hard to force through one Gateway chat turn.
  const probe = {
    mode: "openclaw-plugin-api-hook-probe",
    note:
      "This probe executes handlers registered by the plugin's OpenClaw plugin API entry. It is not a substitute for a model-driven gateway chat turn.",
    importResolution: undefined,
    registeredHooks: [],
    logs: [],
    cases: [],
  };
  const importRoot = await prepareHookProbeImportRoot({ openclawBin, pluginRoot, workdir });
  probe.importResolution = {
    installedPluginRoot: pluginRoot,
    importRoot: importRoot.root,
    openclawPackageRoot: importRoot.openclawPackageRoot,
  };
  const capture = await capturePluginHooks({
    pluginConfig: {
      promptScanBlock: true,
      piiScanUserInput: true,
      codeScanRequireApproval: true,
      capabilities: {
        "pii-scan-user-input": { enableBlock: true },
        "skill-ledger": { policy: "warn" },
      },
    },
    pluginRoot: importRoot.root,
    openclawVersion,
  });
  probe.registeredHooks = capture.hooks.map((hook) => ({
    hookName: hook.hookName,
    priority: hook.priority,
  }));
  probe.logs.push(...capture.logs);

  const skillDir = path.join(workdir, "fixtures", "skills", "pilot-skill");
  await fs.mkdir(skillDir, { recursive: true });
  await fs.writeFile(
    path.join(skillDir, "SKILL.md"),
    "# Pilot Skill\n\nThis fixture is used by the OpenClaw pilot e2e hook probe.\n",
  );

  // prompt-scan and pii-scan pick their input hook through the same host
  // version check (supportsModelInputGate), so both register the same gate on
  // a given host: before_agent_run on stable >=2026.5.12, before_dispatch
  // otherwise. Derive it from the captured registrations instead of
  // re-implementing version parsing here, so both matrix lanes stay honest.
  const inputHook = capture.hooks.some((hook) => hook.hookName === "before_agent_run")
    ? "before_agent_run" : "before_dispatch";
  probe.inputHook = inputHook;
  const normalCases = [
    // These cases cover every registered capability at least once. They are not
    // policy acceptance by themselves; Gateway matrix cases above own that.
    {
      name: `prompt-scan-${inputHook}`,
      hookName: inputHook,
      // Dual-shape event: model-entry fields (prompt/messages) for the
      // before_agent_run gate, inbound fields (content/body) for legacy hosts.
      event: {
        prompt: "hello from the OpenClaw latest pilot",
        messages: [],
        content: "hello from the OpenClaw latest pilot",
        body: "hello from the OpenClaw latest pilot",
        senderId: "pilot-user",
        sessionKey: "pilot-session",
        isGroup: false,
      },
      ctx: beforeDispatchCtx(),
    },
    {
      name: `pii-scan-${inputHook}`,
      hookName: inputHook,
      event: {
        prompt: "Contact me at alice@example.com for the pilot.",
        messages: [],
        content: "Contact me at alice@example.com for the pilot.",
        body: "Contact me at alice@example.com for the pilot.",
        senderId: "pilot-user",
        sessionKey: "pilot-session",
        isGroup: false,
      },
      ctx: beforeDispatchCtx(),
    },
    {
      name: "scan-code-before-tool-call",
      hookName: "before_tool_call",
      event: {
        toolName: "exec",
        params: { command: "openclaw plugins disable agent-sec" },
        runId: "pilot-run",
        sessionId: "pilot-session",
        toolCallId: "pilot-tool-code",
      },
      ctx: toolCtx(repoRoot, "pilot-tool-code", "exec"),
    },
    {
      name: "skill-ledger-before-tool-call",
      hookName: "before_tool_call",
      event: {
        toolName: "read",
        params: { file_path: path.join(skillDir, "SKILL.md") },
        runId: "pilot-run",
        sessionId: "pilot-session",
        toolCallId: "pilot-tool-skill",
      },
      ctx: toolCtx(repoRoot, "pilot-tool-skill", "read"),
    },
    {
      name: "observability-llm-input",
      hookName: "llm_input",
      event: {
        runId: "pilot-run",
        sessionId: "pilot-session",
        provider: "pilot-provider",
        model: "pilot-model",
        systemPrompt: "system",
        prompt: "hello",
        historyMessages: [{ role: "user", content: "hello" }],
        imagesCount: 0,
      },
      ctx: agentCtx(repoRoot),
    },
    {
      name: "observability-model-call-started",
      hookName: "model_call_started",
      event: {
        runId: "pilot-run",
        callId: "pilot-call",
        sessionKey: "pilot-session",
        sessionId: "pilot-session",
        provider: "pilot-provider",
        model: "pilot-model",
        api: "responses",
        transport: "http",
      },
      ctx: agentCtx(repoRoot),
    },
    {
      name: "observability-model-call-ended",
      hookName: "model_call_ended",
      event: {
        runId: "pilot-run",
        callId: "pilot-call",
        sessionKey: "pilot-session",
        sessionId: "pilot-session",
        provider: "pilot-provider",
        model: "pilot-model",
        api: "responses",
        transport: "http",
        durationMs: 25,
        outcome: "completed",
      },
      ctx: agentCtx(repoRoot),
    },
    {
      name: "pii-and-observability-llm-output",
      hookName: "llm_output",
      event: {
        runId: "pilot-run",
        sessionId: "pilot-session",
        provider: "pilot-provider",
        model: "pilot-model",
        resolvedRef: "pilot-provider/pilot-model",
        assistantTexts: ["The pilot finished."],
        lastAssistant: "The pilot finished.",
        usage: { input: 8, output: 4, total: 12 },
      },
      ctx: agentCtx(repoRoot),
    },
    {
      name: "observability-after-tool-call",
      hookName: "after_tool_call",
      event: {
        toolName: "exec",
        params: { command: "echo ok" },
        runId: "pilot-run",
        sessionId: "pilot-session",
        toolCallId: "pilot-tool-after",
        result: { content: "ok" },
        durationMs: 10,
      },
      ctx: toolCtx(repoRoot, "pilot-tool-after", "exec"),
    },
    {
      name: "observability-agent-end",
      hookName: "agent_end",
      event: {
        runId: "pilot-run",
        success: true,
        durationMs: 50,
        messages: [
          { role: "user", content: [{ type: "text", text: "hello" }] },
          { role: "assistant", content: [{ type: "text", text: "done" }] },
        ],
      },
      ctx: agentCtx(repoRoot),
    },
  ];

  for (const testCase of normalCases) {
    probe.cases.push(await invokeCapturedHookCase(capture, testCase));
  }

  if (!skipFailureProbes) {
    // Negative probes must fail open: missing, broken, invalid, or slow CLI
    // behavior should be recorded without throwing from plugin hooks. They ride
    // the input gate hook selected above so both matrix lanes exercise them.
    const missingCliEnv = {
      ...env,
      PATH: await makeEmptyBinDir(workdir, "missing-cli-bin"),
    };
    probe.cases.push(
      await invokeCapturedHookCase(capture, {
        name: "failure-missing-agent-sec-cli",
        hookName: inputHook,
        event: {
          prompt: "hello while agent-sec-cli is absent",
          messages: [],
          content: "hello while agent-sec-cli is absent",
          body: "hello while agent-sec-cli is absent",
          senderId: "pilot-user",
          sessionKey: "pilot-session",
        },
        ctx: beforeDispatchCtx(),
        env: missingCliEnv,
      }),
    );

    probe.cases.push(
      await invokeCapturedHookCase(capture, {
        name: "failure-agent-sec-cli-nonzero",
        hookName: inputHook,
        event: {
          prompt: "hello with nonzero CLI",
          messages: [],
          content: "hello with nonzero CLI",
          body: "hello with nonzero CLI",
          senderId: "pilot-user",
          sessionKey: "pilot-session",
        },
        ctx: beforeDispatchCtx(),
        env: await makeFakeCliEnv(env, workdir, "nonzero", `#!/usr/bin/env bash
echo "pilot nonzero" >&2
exit 42
`),
      }),
    );

    probe.cases.push(
      await invokeCapturedHookCase(capture, {
        name: "failure-agent-sec-cli-invalid-json",
        hookName: inputHook,
        event: {
          prompt: "hello with invalid JSON",
          messages: [],
          content: "hello with invalid JSON",
          body: "hello with invalid JSON",
          senderId: "pilot-user",
          sessionKey: "pilot-session",
        },
        ctx: beforeDispatchCtx(),
        env: await makeFakeCliEnv(env, workdir, "invalid-json", `#!/usr/bin/env bash
printf '{not-json'
`),
      }),
    );

    probe.cases.push(
      await invokeCapturedHookCase(capture, {
        name: "failure-agent-sec-cli-timeout",
        hookName: inputHook,
        event: {
          prompt: "hello with timeout CLI",
          messages: [],
          content: "hello with timeout CLI",
          body: "hello with timeout CLI",
          senderId: "pilot-user",
          sessionKey: "pilot-session",
        },
        ctx: beforeDispatchCtx(),
        env: await makeFakeCliEnv(env, workdir, "timeout", `#!/usr/bin/env bash
sleep 20
`),
      }),
    );
  }

  await sleep(1_000);
  const probeFile = path.join(logsDir, "hook-probe.json");
  await fs.writeFile(probeFile, `${JSON.stringify(probe, null, 2)}\n`);
  probe.resultFile = probeFile;
  return probe;
}

async function prepareHookProbeImportRoot({ openclawBin, pluginRoot, workdir }) {
  // Direct imports run outside the OpenClaw host. Build an isolated module root
  // that uses the installed plugin bits plus the exact host SDK package under test.
  const openclawPackageRoot = await resolveOpenClawPackageRoot(openclawBin);
  const root = path.join(workdir, "hook-probe-import-root");
  await fs.rm(root, { recursive: true, force: true });
  await fs.mkdir(path.join(root, "node_modules"), { recursive: true });
  await fs.cp(path.join(pluginRoot, "dist"), path.join(root, "dist"), { recursive: true });
  await fs.copyFile(path.join(pluginRoot, "package.json"), path.join(root, "package.json"));
  await fs.symlink(openclawPackageRoot, path.join(root, "node_modules", "openclaw"), "dir");
  return { root, openclawPackageRoot };
}

async function resolveOpenClawPackageRoot(openclawBin) {
  if (!openclawBin) {
    throw new Error("hook probe requires the resolved OpenClaw binary path");
  }

  const realBin = await fs.realpath(openclawBin);
  const stat = await fs.stat(realBin);
  let current = stat.isDirectory() ? realBin : path.dirname(realBin);
  for (let depth = 0; depth < 12; depth += 1) {
    const packageName = await readPackageName(path.join(current, "package.json"));
    if (packageName === "openclaw") {
      return current;
    }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }

  throw new Error(`unable to resolve OpenClaw package root from ${openclawBin} (${realBin})`);
}

async function readPackageName(packageJsonPath) {
  try {
    const packageJson = JSON.parse(await fs.readFile(packageJsonPath, "utf8"));
    return typeof packageJson.name === "string" ? packageJson.name : undefined;
  } catch {
    return undefined;
  }
}

export function assertHookProbe(probe) {
  const requiredHookNames = [
    "before_tool_call",
    "after_tool_call",
    "llm_input",
    "llm_output",
    "model_call_started",
    "model_call_ended",
    "agent_end",
  ];
  for (const hookName of requiredHookNames) {
    if (!probe.registeredHooks.some((hook) => hook.hookName === hookName)) {
      throw new Error(`hook probe did not register required hook ${hookName}`);
    }
  }
  // Input-gate hook: prompt-scan and pii-scan each register exactly one input
  // hook via the same host version check — before_agent_run on stable
  // >=2026.5.12 hosts, before_dispatch on older/prerelease hosts. Exactly one
  // of the two must appear; both at once means the capabilities disagreed on
  // the host version, and neither means input scanning is not wired up.
  const registeredHookNames = new Set(
    probe.registeredHooks.map((hook) => hook.hookName),
  );
  const inputGateHooks = ["before_agent_run", "before_dispatch"].filter((name) =>
    registeredHookNames.has(name),
  );
  if (inputGateHooks.length !== 1) {
    throw new Error(
      `hook probe expected exactly one input gate hook (before_agent_run on stable >=2026.5.12, before_dispatch on older hosts), registered: ${
        inputGateHooks.join(", ") || "none"
      }`,
    );
  }
  for (const testCase of probe.cases) {
    if (testCase.matchedHandlers <= 0) {
      throw new Error(`hook probe case ${testCase.name} matched no handlers`);
    }
    const unexpectedError = testCase.results.find((item) => item.error);
    if (unexpectedError) {
      throw new Error(
        `hook probe case ${testCase.name} failed: ${JSON.stringify(unexpectedError.error)}`,
      );
    }
  }
}

async function capturePluginHooks({ pluginConfig, pluginRoot, openclawVersion }) {
  // Import dist with a cache-busting query so repeated pilot runs in the same
  // Node process cannot reuse stale plugin registration state.
  const distEntry = path.join(pluginRoot, "dist", "index.js");
  const moduleUrl = `${pathToFileURL(distEntry).href}?pilot=${Date.now()}`;
  const entry = (await import(moduleUrl)).default;
  if (!entry || typeof entry.register !== "function") {
    throw new Error(`plugin entry does not expose register(api): ${distEntry}`);
  }

  const hooks = [];
  const logs = [];
  const api = {
    pluginConfig,
    runtime: { version: openclawVersion },
    logger: {
      info: (message) => logs.push(`[INFO] ${message}`),
      warn: (message) => logs.push(`[WARN] ${message}`),
      error: (message) => logs.push(`[ERROR] ${message}`),
      debug: (message) => logs.push(`[DEBUG] ${message}`),
    },
    on: (hookName, handler, opts) => {
      hooks.push({
        hookName,
        handler,
        priority: opts?.priority ?? 0,
      });
    },
  };
  entry.register(api);
  hooks.sort((left, right) => right.priority - left.priority);
  return { hooks, logs };
}

async function invokeCapturedHookCase(capture, testCase) {
  const startedAt = Date.now();
  const matchingHooks = capture.hooks.filter((hook) => hook.hookName === testCase.hookName);
  const results = [];
  const restoreEnv = testCase.env ? applyProcessEnvOverlay(testCase.env) : undefined;
  try {
    for (const hook of matchingHooks) {
      const hookStartedAt = Date.now();
      try {
        const value = await withTimeout(
          Promise.resolve(hook.handler(testCase.event, testCase.ctx)),
          DEFAULT_HOOK_TIMEOUT_MS,
          `${testCase.name}:${testCase.hookName}`,
        );
        results.push({
          hookName: hook.hookName,
          priority: hook.priority,
          durationMs: Date.now() - hookStartedAt,
          result: normalizeJsonValue(value),
        });
      } catch (error) {
        results.push({
          hookName: hook.hookName,
          priority: hook.priority,
          durationMs: Date.now() - hookStartedAt,
          error: serializeError(error),
        });
      }
    }
  } finally {
    restoreEnv?.();
  }
  return {
    name: testCase.name,
    hookName: testCase.hookName,
    matchedHandlers: matchingHooks.length,
    durationMs: Date.now() - startedAt,
    results,
  };
}

function applyProcessEnvOverlay(env) {
  // Failure probes need a temporary PATH because plugin code reads process.env
  // at hook invocation time. Mutate keys in place instead of replacing the
  // global object so modules holding a process.env reference stay valid.
  const previous = new Map();
  for (const [key, value] of Object.entries(env)) {
    previous.set(
      key,
      Object.prototype.hasOwnProperty.call(process.env, key) ? process.env[key] : undefined,
    );
    if (value === undefined) {
      delete process.env[key];
    } else {
      process.env[key] = String(value);
    }
  }
  return () => {
    for (const [key, value] of previous) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  };
}

function beforeDispatchCtx() {
  return {
    channelId: "qa-channel",
    accountId: "pilot-account",
    conversationId: "pilot-conversation",
    sessionKey: "pilot-session",
    senderId: "pilot-user",
  };
}

function agentCtx(repoRoot) {
  return {
    channelId: "qa-channel",
    channel: "qa-channel",
    sessionKey: "pilot-session",
    sessionId: "pilot-session",
    runId: "pilot-run",
    agentId: "pilot-agent",
    workspaceDir: repoRoot,
  };
}

function toolCtx(repoRoot, toolCallId, toolName) {
  return {
    ...agentCtx(repoRoot),
    toolName,
    toolCallId,
  };
}

async function makeEmptyBinDir(workdir, name) {
  const dir = path.join(workdir, "fake-bin", name);
  await fs.mkdir(dir, { recursive: true });
  return dir;
}

async function makeFakeCliEnv(env, workdir, name, script) {
  const dir = await makeEmptyBinDir(workdir, name);
  await writeExecutable(path.join(dir, "agent-sec-cli"), script);
  return {
    ...env,
    PATH: `${dir}${path.delimiter}${env.PATH ?? ""}`,
  };
}
