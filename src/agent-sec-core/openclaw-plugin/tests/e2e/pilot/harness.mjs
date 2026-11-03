import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import fs from "node:fs/promises";
import net from "node:net";
import path from "node:path";

import {
  parseJsonFromOutput,
  redactArgs,
  sleep,
  slugify,
  withTimeout,
} from "./common.mjs";
import { StepError, formatError } from "./errors.mjs";

// The harness owns stateful mechanics shared by pilot-style e2e tests: command
// logs, child processes, Gateway RPC calls, and the final evidence file.
export function createPilotHarness({
  defaultCommandTimeoutMs,
  pluginRoot,
  result,
  startedProcesses,
  startedServers,
}) {
  let gatewayCommandEnv = process.env;

  async function runRequiredStep(name, command, commandArgs, options = {}) {
    const step = await runCommand(name, command, commandArgs, options);
    if (step.exitCode !== 0) {
      throw new StepError(name, `command failed with exit ${step.exitCode}`, step);
    }
    return step;
  }

  async function runCommand(name, command, commandArgs, options = {}) {
    const startedAt = new Date().toISOString();
    const stdoutChunks = [];
    const stderrChunks = [];
    const stdoutLog = path.join(result.logsDir, `${slugify(name)}.stdout.log`);
    const stderrLog = path.join(result.logsDir, `${slugify(name)}.stderr.log`);
    const timeoutMs = options.timeoutMs ?? defaultCommandTimeoutMs;

    const recordedArgs = redactArgs(commandArgs);
    console.log(`[pilot] ${name}: ${command} ${recordedArgs.join(" ")}`);

    // Keep stdout/stderr on disk even for successful commands; the result JSON
    // only stores paths so large OpenClaw logs do not bloat the evidence file.
    const step = await new Promise((resolve) => {
      const child = spawn(command, commandArgs, {
        cwd: options.cwd ?? pluginRoot,
        env: options.env ?? process.env,
        detached: process.platform !== "win32",
        stdio: ["pipe", "pipe", "pipe"],
      });
      const processGroupId = process.platform !== "win32" ? child.pid : undefined;
      let timedOut = false;
      const stdoutStream = createWriteStream(stdoutLog);
      const stderrStream = createWriteStream(stderrLog);
      const timer = setTimeout(() => {
        timedOut = true;
        signalChildProcess(child, processGroupId, "SIGTERM");
        setTimeout(() => {
          if (
            (processGroupId !== undefined && processGroupExists(processGroupId)) ||
            (processGroupId === undefined && !hasChildExited(child))
          ) {
            signalChildProcess(child, processGroupId, "SIGKILL");
          }
        }, 5_000).unref();
      }, timeoutMs);
      timer.unref();

      if (options.stdin !== undefined) {
        child.stdin.end(options.stdin);
      } else {
        child.stdin.end();
      }
      child.stdout.on("data", (chunk) => {
        stdoutChunks.push(chunk);
        stdoutStream.write(chunk);
      });
      child.stderr.on("data", (chunk) => {
        stderrChunks.push(chunk);
        stderrStream.write(chunk);
      });
      child.on("error", (error) => {
        clearTimeout(timer);
        stdoutStream.end();
        stderrStream.end();
        const finishedAt = new Date().toISOString();
        resolve({
          name,
          command,
          args: recordedArgs,
          exitCode: 127,
          signal: undefined,
          timedOut,
          startedAt,
          finishedAt,
          durationMs: Date.parse(finishedAt) - Date.parse(startedAt),
          stdout: Buffer.concat(stdoutChunks).toString("utf8"),
          stderr: String(error),
          stdoutLog,
          stderrLog,
        });
      });
      child.on("close", (exitCode, signal) => {
        clearTimeout(timer);
        stdoutStream.end();
        stderrStream.end();
        const finishedAt = new Date().toISOString();
        resolve({
          name,
          command,
          args: recordedArgs,
          exitCode: timedOut ? 124 : (exitCode ?? 1),
          signal: signal ?? undefined,
          timedOut,
          startedAt,
          finishedAt,
          durationMs: Date.parse(finishedAt) - Date.parse(startedAt),
          stdout: Buffer.concat(stdoutChunks).toString("utf8"),
          stderr: Buffer.concat(stderrChunks).toString("utf8"),
          stdoutLog,
          stderrLog,
        });
      });
    });

    result.steps.push({
      name,
      command,
      args: recordedArgs,
      exitCode: step.exitCode,
      signal: step.signal,
      timedOut: step.timedOut,
      startedAt: step.startedAt,
      finishedAt: step.finishedAt,
      durationMs: step.durationMs,
      stdoutLog,
      stderrLog,
    });
    return step;
  }

  function startProcess(name, command, commandArgs, options = {}) {
    const stdoutLog = path.join(result.logsDir, `${slugify(name)}.stdout.log`);
    const stderrLog = path.join(result.logsDir, `${slugify(name)}.stderr.log`);
    const recordedArgs = redactArgs(commandArgs);
    console.log(`[pilot] start ${name}: ${command} ${recordedArgs.join(" ")}`);
    const child = spawn(command, commandArgs, {
      cwd: options.cwd ?? pluginRoot,
      env: options.env ?? process.env,
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdoutStream = createWriteStream(stdoutLog);
    const stderrStream = createWriteStream(stderrLog);
    child.stdout.pipe(stdoutStream);
    child.stderr.pipe(stderrStream);
    const proc = {
      name,
      child,
      processGroupId: process.platform !== "win32" ? child.pid : undefined,
      stdoutLog,
      stderrLog,
      startedAt: new Date().toISOString(),
    };
    startedProcesses.push(proc);
    result.steps.push({
      name,
      command,
      args: recordedArgs,
      process: true,
      startedAt: proc.startedAt,
      stdoutLog,
      stderrLog,
    });
    child.once("exit", (code, signal) => {
      proc.exitCode = code;
      proc.signal = signal;
      proc.finishedAt = new Date().toISOString();
    });
    child.once("error", (error) => {
      proc.error = String(error);
      proc.finishedAt = new Date().toISOString();
    });
    return proc;
  }

  async function startOpenClawGateway({ env, gatewayPort, gatewayToken, gatewayTimeoutMs, reason }) {
    gatewayCommandEnv = env;
    const processName = reason === "initial" ? "openclaw-gateway" : `openclaw-gateway-${reason}`;
    const processRef = startProcess(
      processName,
      "openclaw",
      [
        "gateway",
        "run",
        "--dev",
        "--allow-unconfigured",
        "--auth",
        "token",
        "--token",
        gatewayToken,
        "--bind",
        "loopback",
        "--port",
        String(gatewayPort),
        "--ws-log",
        "compact",
      ],
      { cwd: pluginRoot, env },
    );
    processRef.gatewayPort = gatewayPort;
    let health;
    try {
      health = await waitForGatewayHealth(`ws://127.0.0.1:${gatewayPort}`, {
        env,
        processRef,
        token: gatewayToken,
        timeoutMs: gatewayTimeoutMs,
      });
    } catch (error) {
      if (error && typeof error === "object") {
        error.gatewayProcess = processRef;
      }
      throw error;
    }
    await sleep(1_000);
    assertProcessStillRunning(processRef);
    return { process: processRef, health };
  }

  async function stopStartedProcess(proc) {
    if (!proc) return;
    await signalStartedProcess(proc, "SIGTERM");
    try {
      await withTimeout(waitForStartedProcessStop(proc), 5_000, `stop ${proc.name}`);
    } catch {
      await signalStartedProcess(proc, "SIGKILL");
      await withTimeout(waitForStartedProcessStop(proc), 5_000, `kill ${proc.name}`).catch(
        () => {},
      );
    }
  }

  async function waitForDaemonHealth(socketPath, { processRef, timeoutMs }) {
    const deadline = Date.now() + timeoutMs;
    let lastError;
    while (Date.now() < deadline) {
      assertProcessStillRunning(processRef);
      try {
        const response = await callDaemonHealth(socketPath);
        if (response?.ok === true) {
          return response;
        }
        lastError = new Error(`daemon.health returned ${JSON.stringify(response)}`);
      } catch (error) {
        lastError = error;
      }
      await sleep(250);
    }
    throw new Error(`agent-sec-daemon did not become healthy: ${formatError(lastError)}`);
  }

  function callDaemonHealth(socketPath) {
    return new Promise((resolve, reject) => {
      const client = net.createConnection(socketPath);
      let data = "";
      const timer = setTimeout(() => {
        client.destroy();
        reject(new Error("daemon.health timed out"));
      }, 2_000);
      client.on("connect", () => {
        client.write(
          `${JSON.stringify({
            id: "pilot-daemon-health",
            method: "daemon.health",
            caller: "openclaw-plugin-e2e-pilot",
          })}\n`,
        );
      });
      client.on("data", (chunk) => {
        data += chunk.toString("utf8");
        if (data.includes("\n")) {
          clearTimeout(timer);
          client.end();
          try {
            resolve(JSON.parse(data.trim().split(/\n/u)[0]));
          } catch (error) {
            reject(error);
          }
        }
      });
      client.on("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
    });
  }

  async function waitForGatewayHealth(gatewayUrl, { env, processRef, token, timeoutMs }) {
    const deadline = Date.now() + timeoutMs;
    let lastError;
    while (Date.now() < deadline) {
      assertProcessStillRunning(processRef);
      const step = await runCommand(
        "openclaw-gateway-health",
        "openclaw",
        [
          "gateway",
          "health",
          "--url",
          gatewayUrl,
          "--json",
          "--timeout",
          "1500",
          "--token",
          token,
        ],
        { cwd: pluginRoot, env, timeoutMs: 5_000 },
      );
      if (step.exitCode === 0) {
        try {
          return parseJsonFromOutput(step.stdout);
        } catch (error) {
          lastError = error;
        }
      } else {
        lastError = new StepError("openclaw-gateway-health", "gateway health failed", step);
      }
      await sleep(1_000);
    }
    throw new Error(`OpenClaw gateway did not become healthy: ${formatError(lastError)}`);
  }

  function assertProcessStillRunning(processRef) {
    if (!processRef) return;
    if (hasChildExited(processRef.child)) {
      throw new Error(
        `${processRef.name} exited early with code=${processRef.exitCode} signal=${processRef.signal}; stdout=${processRef.stdoutLog} stderr=${processRef.stderrLog}`,
      );
    }
  }

  async function callGatewayRpc(
    stepName,
    method,
    params,
    {
      env = gatewayCommandEnv,
      gatewayToken,
      gatewayUrl,
      maxAttempts = 3,
      retryDelayBaseMs = 2_000,
      timeoutMs,
    },
  ) {
    const stepTimeoutMs = timeoutMs ?? defaultCommandTimeoutMs;
    let lastStep;
    for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
      const attemptStepName = attempt === 1 ? stepName : `${stepName}-retry-${attempt}`;
      // Use OpenClaw's own Gateway client via `gateway call` instead of hand-rolling
      // the WebSocket handshake. Older gateways bind operator scopes to a signed
      // device identity, which the CLI handles consistently across versions.
      const step = await runCommand(
        attemptStepName,
        "openclaw",
        [
          "gateway",
          "call",
          method,
          "--url",
          gatewayUrl,
          "--token",
          gatewayToken,
          "--json",
          "--timeout",
          String(stepTimeoutMs),
          "--params",
          JSON.stringify(params ?? {}),
        ],
        { env, timeoutMs: stepTimeoutMs + 5_000 },
      );
      if (step.exitCode === 0) {
        return parseJsonFromOutput(step.stdout);
      }
      lastStep = step;
      const approvedPairingUpgrade = isPairingScopeUpgradeFailure(step)
        ? await approveLocalGatewayCliScopeUpgrade(gatewayCommandEnv, result)
        : undefined;
      if (!approvedPairingUpgrade && (!isTransientGatewayCallFailure(step) || attempt === maxAttempts)) {
        break;
      }
      await sleep(retryDelayBaseMs * attempt);
    }
    throw new StepError(stepName, `gateway RPC ${method} failed`, lastStep);
  }

  async function stopAllProcesses() {
    for (const proc of [...startedProcesses].reverse()) {
      await signalStartedProcess(proc, "SIGTERM");
      try {
        await withTimeout(waitForStartedProcessStop(proc), 5_000, `stop ${proc.name}`);
      } catch {
        await signalStartedProcess(proc, "SIGKILL");
        await withTimeout(waitForStartedProcessStop(proc), 5_000, `kill ${proc.name}`).catch(
          () => {},
        );
      }
    }
    for (const serverRef of [...startedServers].reverse()) {
      await closeServer(serverRef).catch(() => {});
    }
  }

  async function writeResultFile() {
    if (!result.workdir) return;
    const resultFile = path.join(result.workdir, "pilot-result.json");
    result.resultFile = resultFile;
    await fs.mkdir(result.workdir, { recursive: true });
    await fs.writeFile(resultFile, `${JSON.stringify(result, null, 2)}\n`);
    console.log(`[pilot] result: ${resultFile}`);
  }

  return {
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
  };
}

function assertRuntimeLoaded(data) {
  const status = data?.plugin?.status;
  if (status !== "loaded") {
    throw new Error(`runtime inspect status is ${JSON.stringify(status)}, expected "loaded"`);
  }
}

function isTransientGatewayCallFailure(step) {
  const text = `${step?.stdout ?? ""}\n${step?.stderr ?? ""}`;
  return /gateway closed|ECONNREFUSED|ECONNRESET|WebSocket error|socket hang up|abnormal closure/iu.test(
    text,
  );
}

function isPairingScopeUpgradeFailure(step) {
  const text = `${step?.stdout ?? ""}\n${step?.stderr ?? ""}`;
  return /scope upgrade pending approval|pairing required: device is asking for more scopes/iu.test(
    text,
  );
}

async function approveLocalGatewayCliScopeUpgrade(env, result) {
  if (!env?.AGENT_SEC_OPENCLAW_PILOT_CLI_LOG) {
    return undefined;
  }
  const stateDir = env.OPENCLAW_STATE_DIR;
  if (!stateDir) {
    return undefined;
  }
  const pendingPath = path.join(stateDir, "devices", "pending.json");
  const pairedPath = path.join(stateDir, "devices", "paired.json");
  const deviceAuthPath = path.join(stateDir, "identity", "device-auth.json");
  let pending;
  let paired;
  let deviceAuth;
  try {
    [pending, paired, deviceAuth] = await Promise.all([
      readJsonFileOrDefault(pendingPath, {}),
      readJsonFileOrDefault(pairedPath, {}),
      readJsonFileOrDefault(deviceAuthPath, {}),
    ]);
  } catch (error) {
    result.gatewayPairingApprovals ??= [];
    result.gatewayPairingApprovals.push({
      approved: false,
      reason: "read-failed",
      error: formatError(error),
    });
    return undefined;
  }

  const deviceId = typeof deviceAuth.deviceId === "string" ? deviceAuth.deviceId : "";
  const device = deviceId ? paired[deviceId] : undefined;
  const pendingEntries = Object.values(pending).filter((request) => {
    return (
      request &&
      request.deviceId === deviceId &&
      request.clientId === "cli" &&
      request.clientMode === "cli" &&
      request.role === "operator" &&
      Array.isArray(request.scopes) &&
      request.scopes.length > 0 &&
      (!device?.publicKey || request.publicKey === device.publicKey)
    );
  });
  if (!device || pendingEntries.length === 0 || !device.tokens?.operator || !deviceAuth.tokens?.operator) {
    result.gatewayPairingApprovals ??= [];
    result.gatewayPairingApprovals.push({
      approved: false,
      reason: "no-matching-cli-scope-upgrade",
      deviceId: deviceId || undefined,
      pendingCount: Object.keys(pending).length,
    });
    return undefined;
  }

  const requestedScopes = mergeStringLists(...pendingEntries.map((request) => request.scopes));
  // Proactively grant all CLI default operator scopes so that subsequent
  // gateway RPC calls (e.g. plugin.approval.list which needs operator.approvals)
  // do not require further scope upgrades. The gateway reads pairing state from
  // disk on each connection, so this takes effect immediately for the next call
  // and persists across gateway restarts.
  const approvedScopes = mergeStringLists(
    device.approvedScopes,
    device.scopes,
    requestedScopes,
    "operator.admin",
    "operator.read",
    "operator.write",
    "operator.approvals",
    "operator.pairing",
    "operator.talk.secrets",
  );
  device.scopes = approvedScopes;
  device.approvedScopes = approvedScopes;
  device.roles = mergeStringLists(device.roles, device.role, "operator");
  device.tokens.operator.scopes = approvedScopes;
  deviceAuth.tokens.operator.scopes = approvedScopes;
  for (const request of pendingEntries) {
    delete pending[request.requestId];
  }
  await Promise.all([
    writeJsonFile(pendingPath, pending),
    writeJsonFile(pairedPath, paired),
    writeJsonFile(deviceAuthPath, deviceAuth),
  ]);

  const approval = {
    approved: true,
    deviceId,
    requestIds: pendingEntries.map((request) => request.requestId),
    requestedScopes,
    approvedScopes,
  };
  result.gatewayPairingApprovals ??= [];
  result.gatewayPairingApprovals.push(approval);
  return approval;
}

function summarizeRuntimeInspect(data) {
  const hooks = new Set();
  const text = JSON.stringify(data);
  for (const hookName of [
    "before_dispatch",
    "before_agent_run",
    "before_tool_call",
    "after_tool_call",
    "llm_input",
    "llm_output",
    "model_call_started",
    "model_call_ended",
    "agent_end",
  ]) {
    if (text.includes(hookName)) {
      hooks.add(hookName);
    }
  }
  return {
    plugin: data.plugin,
    hookNamesFound: [...hooks].sort(),
    diagnostics: data.diagnostics,
  };
}

async function parseNpmPackArtifact(stdout, artifactsDir) {
  try {
    const parsed = JSON.parse(stdout);
    const first = Array.isArray(parsed) ? parsed[0] : parsed;
    if (first?.filename) {
      return path.join(artifactsDir, first.filename);
    }
  } catch {
    // Fall through to a conservative filename search.
  }
  const match = stdout.match(/agent-sec-openclaw-plugin-[^\s]+\.tgz/u);
  if (match) {
    return path.join(artifactsDir, match[0]);
  }

  const files = await fs.readdir(artifactsDir);
  const artifacts = files
    .filter((file) => /^agent-sec-openclaw-plugin-[^\s/]+\.tgz$/u.test(file))
    .sort();
  if (artifacts.length === 1) {
    return path.join(artifactsDir, artifacts[0]);
  }
  if (artifacts.length > 1) {
    throw new Error(
      `npm pack did not report an artifact and ${artifactsDir} contains multiple candidates: ${artifacts.join(", ")}`,
    );
  }
  return undefined;
}

async function readJsonFileOrDefault(filePath, defaultValue) {
  try {
    return JSON.parse(await fs.readFile(filePath, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") {
      return defaultValue;
    }
    throw error;
  }
}

async function writeJsonFile(filePath, value) {
  await fs.writeFile(filePath, `${JSON.stringify(value, null, 2)}\n`);
}

function mergeStringLists(...items) {
  const out = new Set();
  for (const item of items) {
    if (typeof item === "string") {
      if (item.trim()) {
        out.add(item.trim());
      }
      continue;
    }
    if (!Array.isArray(item)) {
      continue;
    }
    for (const value of item) {
      if (typeof value === "string" && value.trim()) {
        out.add(value.trim());
      }
    }
  }
  return [...out];
}

function waitForExit(child) {
  return new Promise((resolve, reject) => {
    if (hasChildExited(child)) {
      resolve();
      return;
    }
    child.once("exit", resolve);
    child.once("error", reject);
  });
}

async function waitForStartedProcessStop(proc) {
  if (proc.processGroupId === undefined && proc.gatewayPort === undefined) {
    await waitForExit(proc.child);
    return;
  }
  while (
    (proc.processGroupId !== undefined && processGroupExists(proc.processGroupId)) ||
    (proc.gatewayPort !== undefined && (await listeningPidsOnPort(proc.gatewayPort)).length > 0)
  ) {
    await sleep(100);
  }
}

async function signalStartedProcess(proc, signal) {
  signalChildProcess(proc.child, proc.processGroupId, signal);
  if (proc.gatewayPort !== undefined) {
    for (const pid of await listeningPidsOnPort(proc.gatewayPort)) {
      signalPid(pid, signal);
    }
  }
}

function signalChildProcess(child, processGroupId, signal) {
  try {
    if (processGroupId !== undefined) {
      process.kill(-processGroupId, signal);
    } else if (!hasChildExited(child)) {
      child.kill(signal);
    }
  } catch (error) {
    if (error?.code !== "ESRCH") {
      throw error;
    }
  }
}

function processGroupExists(processGroupId) {
  try {
    process.kill(-processGroupId, 0);
    return true;
  } catch (error) {
    if (error?.code === "ESRCH") {
      return false;
    }
    throw error;
  }
}

function signalPid(pid, signal) {
  try {
    process.kill(pid, signal);
  } catch (error) {
    if (error?.code !== "ESRCH") {
      throw error;
    }
  }
}

async function listeningPidsOnPort(port) {
  if (process.platform !== "linux") {
    return [];
  }
  const socketInodes = await listeningSocketInodesOnPort(port);
  if (socketInodes.size === 0) {
    return [];
  }
  const procEntries = await fs.readdir("/proc", { withFileTypes: true });
  const pids = new Set();
  for (const entry of procEntries) {
    if (!entry.isDirectory() || !/^\d+$/u.test(entry.name)) {
      continue;
    }
    if (await processOwnsSocketInode(entry.name, socketInodes)) {
      pids.add(Number(entry.name));
    }
  }
  return [...pids];
}

async function listeningSocketInodesOnPort(port) {
  const inodes = new Set();
  await collectListeningSocketInodes("/proc/net/tcp", port, inodes);
  await collectListeningSocketInodes("/proc/net/tcp6", port, inodes);
  return inodes;
}

async function collectListeningSocketInodes(procNetFile, port, inodes) {
  let content;
  try {
    content = await fs.readFile(procNetFile, "utf8");
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
    return;
  }
  const expectedPortHex = port.toString(16).toUpperCase().padStart(4, "0");
  for (const line of content.trim().split(/\n/u).slice(1)) {
    const fields = line.trim().split(/\s+/u);
    const localAddress = fields[1] ?? "";
    const state = fields[3] ?? "";
    const inode = fields[9] ?? "";
    const localPortHex = localAddress.split(":").at(-1)?.toUpperCase();
    if (localPortHex === expectedPortHex && state === "0A" && inode) {
      inodes.add(inode);
    }
  }
}

async function processOwnsSocketInode(pid, socketInodes) {
  let fds;
  try {
    fds = await fs.readdir(`/proc/${pid}/fd`);
  } catch (error) {
    if (["ENOENT", "EACCES", "EPERM"].includes(error?.code)) {
      return false;
    }
    throw error;
  }
  for (const fd of fds) {
    let target;
    try {
      target = await fs.readlink(`/proc/${pid}/fd/${fd}`);
    } catch (error) {
      if (["ENOENT", "EACCES", "EPERM"].includes(error?.code)) {
        continue;
      }
      throw error;
    }
    const match = target.match(/^socket:\[(\d+)\]$/u);
    if (match && socketInodes.has(match[1])) {
      return true;
    }
  }
  return false;
}

function hasChildExited(child) {
  // Signal-terminated children keep exitCode null, and stale references may
  // reach cleanup after their exit event has already fired.
  return child.exitCode !== null || child.signalCode !== null;
}

function closeServer(serverRef) {
  return new Promise((resolve, reject) => {
    serverRef.server.close((error) => {
      if (error) {
        reject(error);
        return;
      }
      resolve();
    });
  });
}
