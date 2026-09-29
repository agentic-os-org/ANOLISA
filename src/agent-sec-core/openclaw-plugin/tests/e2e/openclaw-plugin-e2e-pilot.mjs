#!/usr/bin/env node
import fs from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  PLUGIN_ID,
  extractVersion,
  findFreePort,
  parseJsonFromOutput,
  readJsonLines,
  readTextIfExists,
} from "./pilot/common.mjs";
import { parseArgs, printHelp, resolveOpenClawBin } from "./pilot/args.mjs";
import { formatError, serializeError } from "./pilot/errors.mjs";
import {
  assertGatewayTrafficProbe,
  assertPolicyMatrix,
  runGatewayPolicyMatrix,
  runGatewayTrafficProbe,
} from "./pilot/gateway-probes.mjs";
import { createPilotHarness } from "./pilot/harness.mjs";
import { assertHookProbe, runHookProbe } from "./pilot/hook-probe.mjs";
import {
  configureGatewayPilotModel,
  startMockModelServer,
} from "./pilot/mock-model.mjs";
import { buildAgentSecCliOverrideConfig, installWrappers } from "./pilot/wrappers.mjs";

// This file is intentionally kept as the top-level orchestration script. The
// heavier test mechanics live under ./pilot so the acceptance flow is readable:
// prepare isolated state, deploy, start Gateway, run probes, write evidence.
const SCRIPT_DIR = path.dirname(fileURLToPath(import.meta.url));
const PLUGIN_ROOT = path.resolve(SCRIPT_DIR, "..", "..");
const REPO_ROOT = path.resolve(PLUGIN_ROOT, "..");
const AGENT_SEC_CLI_PROJECT = path.join(REPO_ROOT, "agent-sec-cli");
const DEFAULT_COMMAND_TIMEOUT_MS = 600_000;
const DEFAULT_GATEWAY_TIMEOUT_MS = 180_000;
const args = parseArgs(process.argv.slice(2));
const startedProcesses = [];
const startedServers = [];

// Every step writes logs under workdir/logs and a compact reference here. The
// final pilot-result.json is the contract consumed by manual review and matrix
// task evidence, so keep it stable and append-only when possible.
const result = {
  schemaVersion: 1,
  task: "OPENCLAW-PLUGIN-E2E-PILOT",
  pilotRunId: undefined,
  status: "running",
  startedAt: new Date().toISOString(),
  finishedAt: undefined,
  repoRoot: REPO_ROOT,
  pluginRoot: PLUGIN_ROOT,
  workdir: undefined,
  artifactsDir: undefined,
  logsDir: undefined,
  versions: {},
  paths: {},
  steps: [],
  daemonHealth: undefined,
  gatewayHealth: undefined,
  gatewayStartAttempts: [],
  install: {},
  mockModel: undefined,
  runtimeInspect: undefined,
  gatewayTrafficProbe: undefined,
  policyMatrix: undefined,
  hookProbe: undefined,
  errors: [],
};

const {
  assertProcessStillRunning,
  assertRuntimeLoaded,
  callGatewayRpc,
  parseNpmPackArtifact,
  runRequiredStep,
  startOpenClawGateway,
  startProcess,
  stopAllProcesses,
  stopStartedProcess,
  summarizeRuntimeInspect,
  waitForDaemonHealth,
  writeResultFile,
} = createPilotHarness({
  defaultCommandTimeoutMs: DEFAULT_COMMAND_TIMEOUT_MS,
  pluginRoot: PLUGIN_ROOT,
  result,
  startedProcesses,
  startedServers,
});
let cleanupPromise;
let handlingTerminationSignal = false;

registerTerminationHandler("SIGTERM");
registerTerminationHandler("SIGINT");

try {
  await runPilot();
  result.status = "passed";
} catch (error) {
  result.status = "failed";
  result.errors.push(serializeError(error));
  console.error(formatError(error));
  process.exitCode = 1;
} finally {
  await finishPilot();
}

async function finishPilot() {
  if (!cleanupPromise) {
    cleanupPromise = (async () => {
      await stopAllProcesses();
      result.finishedAt = new Date().toISOString();
      await writeResultFile();
    })();
  }
  return cleanupPromise;
}

function registerTerminationHandler(signal) {
  process.once(signal, () => {
    void handleTerminationSignal(signal);
  });
}

async function handleTerminationSignal(signal) {
  if (handlingTerminationSignal) return;
  handlingTerminationSignal = true;

  let exitCode = signalExitCode(signal);
  result.status = "failed";
  result.errors.push(serializeError(new Error(`received ${signal}`)));
  process.exitCode = exitCode;
  console.error(`[pilot] received ${signal}; stopping child processes`);

  try {
    await finishPilot();
  } catch (error) {
    exitCode = 1;
    process.exitCode = exitCode;
    console.error(formatError(error));
  } finally {
    process.exit(exitCode);
  }
}

function signalExitCode(signal) {
  if (signal === "SIGINT") return 130;
  if (signal === "SIGTERM") return 143;
  return 1;
}

async function runPilot() {
  if (args.help) {
    printHelp();
    process.exit(0);
  }

  const workdir = args.workdir
    ? path.resolve(args.workdir)
    : process.env.AGENT_SEC_OPENCLAW_PILOT_WORKDIR
      ? path.resolve(process.env.AGENT_SEC_OPENCLAW_PILOT_WORKDIR)
      : await fs.mkdtemp(path.join(os.tmpdir(), "agentsec-openclaw-pilot-"));
  const pilotRunId = `${new Date()
    .toISOString()
    .replace(/[^0-9A-Za-z_.-]/gu, "-")}-${process.pid}`;
  const logsDir = path.join(workdir, "logs");
  const artifactsDir = path.join(workdir, "artifacts");
  const binDir = path.join(workdir, "bin");
  const openclawStateDir = path.join(workdir, "openclaw-state");
  const dataDir = path.join(workdir, "agent-sec-data");
  const xdgDataHome = path.join(workdir, "xdg-data");
  const xdgConfigHome = path.join(workdir, "xdg-config");
  const xdgCacheHome = path.join(workdir, "xdg-cache");
  const daemonSocket = path.join(workdir, "agent-sec-daemon.sock");
  const openclawConfigPath = path.join(openclawStateDir, "openclaw.json");
  const openclawCallsLog = path.join(logsDir, `openclaw-calls-${pilotRunId}.jsonl`);
  const agentSecCliCallsLog = path.join(logsDir, `agent-sec-cli-calls-${pilotRunId}.jsonl`);
  const agentSecCliOverrideFile = path.join(workdir, "agent-sec-cli-overrides.json");

  result.pilotRunId = pilotRunId;
  result.workdir = workdir;
  result.artifactsDir = artifactsDir;
  result.logsDir = logsDir;
  result.paths = {
    binDir,
    openclawStateDir,
    openclawConfigPath,
    daemonSocket,
    dataDir,
    xdgDataHome,
    xdgConfigHome,
    xdgCacheHome,
    openclawCallsLog,
    agentSecCliCallsLog,
    agentSecCliOverrideFile,
  };

  // The daemon requires its socket's parent to be private, including when
  // --workdir names a directory that does not exist yet.
  await fs.mkdir(workdir, { recursive: true, mode: 0o700 });
  await fs.mkdir(logsDir, { recursive: true });
  await fs.mkdir(artifactsDir, { recursive: true });
  await fs.mkdir(binDir, { recursive: true });
  await fs.mkdir(openclawStateDir, { recursive: true });
  await fs.mkdir(dataDir, { recursive: true });
  await fs.mkdir(xdgDataHome, { recursive: true });
  await fs.mkdir(xdgConfigHome, { recursive: true });
  await fs.mkdir(xdgCacheHome, { recursive: true });
  await fs.writeFile(openclawCallsLog, "");
  await fs.writeFile(agentSecCliCallsLog, "");
  // The CLI wrapper reads this file to keep policy-triggering inputs
  // deterministic while passing normal agent-sec-cli calls through.
  await fs.writeFile(
    agentSecCliOverrideFile,
    `${JSON.stringify(buildAgentSecCliOverrideConfig(), null, 2)}\n`,
  );

  // The wrappers make the test hermetic without hiding the real host behavior:
  // openclaw still comes from PATH/--openclaw-bin, while agent-sec-cli calls are
  // logged and only policy-marker inputs get deterministic deny overrides.
  const openclawBin = resolveOpenClawBin(args.openclawBin);
  const wrapperTargets = await installWrappers({
    agentSecCliProject: AGENT_SEC_CLI_PROJECT,
    binDir,
    openclawBin,
    openclawCallsLog,
    agentSecCliBin: args.agentSecCli,
    agentSecDaemonBin: args.agentSecDaemon,
    pluginRoot: PLUGIN_ROOT,
    repoRoot: REPO_ROOT,
  });
  result.paths.agentSecCliLauncher = wrapperTargets.agentSecCli;
  result.paths.agentSecDaemonLauncher = wrapperTargets.agentSecDaemon;

  const baseEnv = {
    ...process.env,
    PATH: `${binDir}${path.delimiter}${process.env.PATH ?? ""}`,
    OPENCLAW_STATE_DIR: openclawStateDir,
    OPENCLAW_CONFIG_PATH: openclawConfigPath,
    AGENT_SEC_DAEMON_SOCKET: daemonSocket,
    AGENT_SEC_DATA_DIR: dataDir,
    AGENT_SEC_OPENCLAW_PILOT_OPENCLAW_LOG: openclawCallsLog,
    AGENT_SEC_OPENCLAW_PILOT_CLI_LOG: agentSecCliCallsLog,
    AGENT_SEC_OPENCLAW_PILOT_CLI_OVERRIDE_FILE: agentSecCliOverrideFile,
    // Bonjour/mDNS discovery is unrelated to this plugin e2e and OpenClaw
    // 2026.4.24 has a reproducible ciao cancellation crash in CI-like hosts.
    OPENCLAW_DISABLE_BONJOUR: "1",
    XDG_DATA_HOME: xdgDataHome,
    XDG_CONFIG_HOME: xdgConfigHome,
    XDG_CACHE_HOME: xdgCacheHome,
    NO_COLOR: "1",
  };

  result.versions.node = process.version;
  await runRequiredStep("npm-version", "npm", ["--version"], { cwd: PLUGIN_ROOT, env: baseEnv });
  const openclawVersion = await runRequiredStep("openclaw-version", "openclaw", ["--version"], {
    cwd: PLUGIN_ROOT,
    env: baseEnv,
  });
  result.versions.openclaw = extractVersion(openclawVersion.stdout) ?? openclawVersion.stdout.trim();

  const agentSecVersion = await runRequiredStep(
    "agent-sec-cli-version",
    "agent-sec-cli",
    ["--version"],
    { cwd: REPO_ROOT, env: baseEnv },
  );
  result.versions.agentSecCli = agentSecVersion.stdout.trim();

  if (args.pluginPackage) {
    result.install.packageArtifact = path.resolve(args.pluginPackage);
    await fs.access(result.install.packageArtifact);
    result.install.prebuiltPackage = true;
  } else {
    await runRequiredStep("agent-sec-plugin-build", "npm", ["run", "build"], {
      cwd: PLUGIN_ROOT,
      env: baseEnv,
      timeoutMs: DEFAULT_COMMAND_TIMEOUT_MS,
    });
    const packResult = await runRequiredStep(
      "agent-sec-plugin-pack",
      "npm",
      ["pack", "--pack-destination", artifactsDir, "--json"],
      { cwd: PLUGIN_ROOT, env: baseEnv, timeoutMs: DEFAULT_COMMAND_TIMEOUT_MS },
    );
    result.install.packageArtifact = await parseNpmPackArtifact(packResult.stdout, artifactsDir);
    result.install.prebuiltPackage = false;
  }
  result.install.packageRoot = await extractPackedPluginPackage({
    artifactsDir,
    env: baseEnv,
    packageArtifact: result.install.packageArtifact,
    runRequiredStep,
  });
  result.install.packageDeployScript = path.join(result.install.packageRoot, "scripts", "deploy.sh");

  const daemon = startProcess("agent-sec-daemon", "agent-sec-daemon", ["serve", "--socket", daemonSocket], {
    cwd: REPO_ROOT,
    env: baseEnv,
  });
  result.daemonHealth = await waitForDaemonHealth(daemonSocket, {
    processRef: daemon,
    timeoutMs: 30_000,
  });

  await runRequiredStep("jq-version", "jq", ["--version"], {
    cwd: result.install.packageRoot,
    env: baseEnv,
  });
  const deployResult = await runRequiredStep(
    "openclaw-plugin-deploy",
    "bash",
    [result.install.packageDeployScript, result.install.packageRoot],
    { cwd: result.install.packageRoot, env: baseEnv, timeoutMs: DEFAULT_COMMAND_TIMEOUT_MS },
  );
  result.install.deployStdoutLog = deployResult.stdoutLog;
  result.install.deployStderrLog = deployResult.stderrLog;
  result.install.openclawInstallInvocation = await findOpenClawPluginInstallInvocation(openclawCallsLog);
  result.install.usedUnsafeInstallFlag = detectDeployUsedUnsafeInstallFlag({
    deployResult,
    installInvocation: result.install.openclawInstallInvocation,
  });

  await runRequiredStep(
    "openclaw-config-enable-pii-block",
    "openclaw",
    [
      "config",
      "set",
      "plugins.entries.agent-sec.config.capabilities.pii-scan-user-input.enableBlock",
      "true",
      "--strict-json",
    ],
    { cwd: result.install.packageRoot, env: baseEnv },
  );
  await runRequiredStep(
    "openclaw-config-skill-ledger-warn",
    "openclaw",
    [
      "config",
      "set",
      "plugins.entries.agent-sec.config.capabilities.skill-ledger.policy",
      '"warn"',
      "--strict-json",
    ],
    { cwd: result.install.packageRoot, env: baseEnv },
  );

  // The mock model is only responsible for deterministic tool-turn behavior;
  // prompts still travel through real OpenClaw Gateway sessions and plugin hooks.
  const mockModel = await startMockModelServer({
    logsDir,
    registerServer: (serverRef) => startedServers.push(serverRef),
  });
  result.paths.mockModelBaseUrl = mockModel.baseUrl;
  result.mockModel = {
    baseUrl: mockModel.baseUrl,
    requestsLog: mockModel.requestsLog,
  };
  // Point OpenClaw at the mock model through normal config so the Gateway uses
  // the same model-selection path as a user-run session.
  await configureGatewayPilotModel({
    env: baseEnv,
    mockModel,
    pluginRoot: result.install.packageRoot,
    runRequiredStep,
  });

  const explicitGatewayPort = args.port ? Number(args.port) : undefined;
  let gatewayPort = explicitGatewayPort ?? await findFreePort();
  let gatewayUrl = buildGatewayUrl(gatewayPort);
  result.paths.gatewayPort = gatewayPort;
  result.paths.gatewayUrl = gatewayUrl;
  let gatewayToken;
  let gatewayProcess;
  const callGatewayRpcWithBaseEnv = (stepName, method, params, options = {}) =>
    callGatewayRpc(stepName, method, params, { ...options, env: baseEnv });
  const setGatewayPort = (port) => {
    gatewayPort = port;
    gatewayUrl = buildGatewayUrl(port);
    result.paths.gatewayPort = gatewayPort;
    result.paths.gatewayUrl = gatewayUrl;
  };
  const restartGateway = async (reason) => {
    if (gatewayProcess) {
      await stopStartedProcess(gatewayProcess);
      gatewayProcess = undefined;
    }
    const maxAttempts = !explicitGatewayPort ? 5 : 1;
    let lastError;
    for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
      if (attempt > 1 || reason !== "initial") {
        setGatewayPort(await findFreePort());
      }
      try {
        // Shared starter used for the initial Gateway process and any explicit
        // restart probes that need a fresh runtime.
        const started = await startOpenClawGateway({
          env: baseEnv,
          gatewayPort,
          gatewayToken,
          gatewayTimeoutMs: Number(args.gatewayTimeoutMs ?? DEFAULT_GATEWAY_TIMEOUT_MS),
          reason,
        });
        gatewayProcess = started.process;
        result.gatewayHealth = started.health;
        result.gatewayStartAttempts.push({
          attempt,
          port: gatewayPort,
          reason,
          status: "started",
        });
        return { gatewayPort, gatewayUrl, process: gatewayProcess };
      } catch (error) {
        lastError = error;
        const portBindFailure = await isGatewayPortBindFailure(error);
        result.gatewayStartAttempts.push({
          attempt,
          error: serializeError(error),
          port: gatewayPort,
          portBindFailure,
          reason,
          status: "failed",
        });
        if (error?.gatewayProcess) {
          await stopStartedProcess(error.gatewayProcess).catch(() => {});
        }
        if (!portBindFailure || attempt === maxAttempts) {
          throw error;
        }
      }
    }
    throw lastError;
  };

  if (!args.skipGateway) {
    gatewayToken =
      args.gatewayToken ?? process.env.AGENT_SEC_OPENCLAW_PILOT_GATEWAY_TOKEN ?? "agent-sec-pilot-token";
    result.paths.gatewayAuth = "token";
    await restartGateway("initial");
  }

  const inspectHelp = await runRequiredStep(
    "openclaw-plugin-inspect-help",
    "openclaw",
    ["plugins", "inspect", "--help"],
    { cwd: result.install.packageRoot, env: baseEnv, timeoutMs: DEFAULT_COMMAND_TIMEOUT_MS },
  );
  const runtimeInspectArgs = inspectHelp.stdout.includes("--runtime")
    ? ["plugins", "inspect", PLUGIN_ID, "--runtime", "--json"]
    : ["plugins", "inspect", PLUGIN_ID, "--json"];
  const runtimeInspect = await runRequiredStep(
    "openclaw-plugin-runtime-inspect",
    "openclaw",
    runtimeInspectArgs,
    { cwd: result.install.packageRoot, env: baseEnv, timeoutMs: DEFAULT_COMMAND_TIMEOUT_MS },
  );
  const runtimeInspectJson = parseJsonFromOutput(runtimeInspect.stdout);
  assertRuntimeLoaded(runtimeInspectJson);
  result.runtimeInspect = summarizeRuntimeInspect(runtimeInspectJson);
  result.runtimeInspect.args = runtimeInspectArgs;
  result.runtimeInspect.rawLog = runtimeInspect.stdoutLog;
  result.install.installedPluginRoot =
    result.runtimeInspect.plugin?.rootDir ?? result.install.packageRoot;

  if (!args.skipGateway) {
    // Happy-path probe: verify one full model-driven Gateway turn reaches the
    // plugin hooks, agent-sec-cli, tool execution, and observability output.
    result.gatewayTrafficProbe = await runGatewayTrafficProbe({
      assertProcessStillRunning,
      callGatewayRpc: callGatewayRpcWithBaseEnv,
      dataDir,
      gatewayToken,
      gatewayUrl,
      logsDir,
      mockModel,
      processRef: gatewayProcess,
      runtimeInspect: result.runtimeInspect,
    });
    assertGatewayTrafficProbe(result.gatewayTrafficProbe);
    // Policy matrix: mutate plugin config, apply it with the version-appropriate
    // strategy (hot reload for verified hosts, Gateway restart for older hosts),
    // then assert behavior from session/model/approval evidence.
    result.policyMatrix = await runGatewayPolicyMatrix({
      callGatewayRpc: callGatewayRpcWithBaseEnv,
      cliLogPath: agentSecCliCallsLog,
      env: baseEnv,
      gatewayToken,
      gatewayUrl,
      getGatewayUrl: () => gatewayUrl,
      logsDir,
      mockModel,
      openclawVersion: result.versions.openclaw,
      pluginRoot: result.install.packageRoot,
      restartGateway,
      runRequiredStep,
    });
    assertPolicyMatrix(result.policyMatrix);
  } else {
    result.gatewayTrafficProbe = {
      skipped: true,
      reason: "--skip-gateway",
    };
    result.policyMatrix = {
      skipped: true,
      reason: "--skip-gateway",
    };
  }

  // Direct hook probe stays as a lower-level diagnostic lane. The Gateway probes
  // are the acceptance signal; this makes hook-level failures easier to isolate.
  result.hookProbe = await runHookProbe({
    env: baseEnv,
    logsDir,
    openclawBin,
    pluginRoot: result.install.installedPluginRoot,
    repoRoot: REPO_ROOT,
    workdir,
    skipFailureProbes: args.skipFailureProbes,
  });

  assertHookProbe(result.hookProbe);
}

async function findOpenClawPluginInstallInvocation(openclawCallsLog) {
  const calls = await readJsonLines(openclawCallsLog);
  const installCalls = calls.filter((call) => {
    const argv = Array.isArray(call?.args) ? call.args : [];
    return argv[0] === "plugins" && argv[1] === "install";
  });
  return installCalls.at(-1);
}

function detectDeployUsedUnsafeInstallFlag({ deployResult, installInvocation }) {
  const argv = Array.isArray(installInvocation?.args) ? installInvocation.args : undefined;
  if (argv) {
    return argv.includes("--dangerously-force-unsafe-install");
  }

  const output = `${deployResult.stdout}\n${deployResult.stderr}`;
  if (output.includes("安装器未暴露 legacy --dangerously-force-unsafe-install")) {
    return false;
  }
  return (
    output.includes("安装器暴露 legacy --dangerously-force-unsafe-install") ||
    output.includes("via --dangerously-force-unsafe-install")
  );
}

function buildGatewayUrl(port) {
  return `ws://127.0.0.1:${port}`;
}

async function isGatewayPortBindFailure(error) {
  const processRef = error?.gatewayProcess;
  const logText = processRef
    ? `${await readTextIfExists(processRef.stdoutLog)}\n${await readTextIfExists(processRef.stderrLog)}`
    : "";
  const text = `${error?.message ?? ""}\n${logText}`;
  return /EADDRINUSE|address already in use|listen .*127\.0\.0\.1/iu.test(text);
}

async function extractPackedPluginPackage({ artifactsDir, env, packageArtifact, runRequiredStep }) {
  if (!packageArtifact) {
    throw new Error("npm pack did not report an agent-sec plugin artifact");
  }
  const extractDir = path.join(artifactsDir, "packed-plugin");
  const packageRoot = path.join(extractDir, "package");
  await fs.rm(extractDir, { recursive: true, force: true });
  await fs.mkdir(extractDir, { recursive: true });
  await runRequiredStep(
    "agent-sec-plugin-extract-pack",
    "tar",
    ["-xzf", packageArtifact, "-C", extractDir],
    { cwd: artifactsDir, env, timeoutMs: DEFAULT_COMMAND_TIMEOUT_MS },
  );
  for (const requiredPath of [
    path.join(packageRoot, "package.json"),
    path.join(packageRoot, "openclaw.plugin.json"),
    path.join(packageRoot, "dist", "index.js"),
    path.join(packageRoot, "scripts", "deploy.sh"),
  ]) {
    try {
      await fs.access(requiredPath);
    } catch {
      throw new Error(`packed plugin artifact is missing ${requiredPath}`);
    }
  }
  await assertPackedPluginEntrypoints(packageRoot);
  return packageRoot;
}

async function assertPackedPluginEntrypoints(packageRoot) {
  const packageManifest = await readJsonFile(path.join(packageRoot, "package.json"));
  const openclawManifest = await readJsonFile(path.join(packageRoot, "openclaw.plugin.json"));
  const entrypointGroups = [
    ["package.json openclaw.extensions", packageManifest?.openclaw?.extensions],
    ["package.json openclaw.runtimeExtensions", packageManifest?.openclaw?.runtimeExtensions],
    ["openclaw.plugin.json extensions", openclawManifest?.extensions],
  ];
  for (const [label, entries] of entrypointGroups) {
    if (!Array.isArray(entries) || entries.length === 0) {
      throw new Error(`packed plugin artifact is missing ${label}`);
    }
    for (const entry of entries) {
      if (typeof entry !== "string" || entry.length === 0) {
        throw new Error(`packed plugin artifact has invalid ${label} entry: ${JSON.stringify(entry)}`);
      }
      const entryPath = path.resolve(packageRoot, entry);
      const relative = path.relative(packageRoot, entryPath);
      if (relative.startsWith("..") || path.isAbsolute(relative)) {
        throw new Error(`packed plugin artifact ${label} escapes package root: ${entry}`);
      }
      try {
        await fs.access(entryPath);
      } catch {
        throw new Error(`packed plugin artifact ${label} points to missing file: ${entry}`);
      }
    }
  }
}

async function readJsonFile(file) {
  return JSON.parse(await fs.readFile(file, "utf8"));
}
