import { existsSync } from "node:fs";
import path from "node:path";

import {
  POLICY_CODE_DENY_COMMAND,
  POLICY_PROMPT_DENY_MARKER,
  shellQuote,
  writeExecutable,
} from "./common.mjs";

export async function installWrappers({
  agentSecCliBin,
  agentSecCliProject,
  agentSecDaemonBin,
  binDir,
  openclawBin,
  openclawCallsLog,
  pluginRoot,
  repoRoot,
}) {
  // Put a private bin directory first on PATH so the pilot controls exactly how
  // openclaw/agent-sec binaries are launched without mutating the user's shell.
  const agentSecCli = resolveAgentSecExecutable({
    envName: "AGENT_SEC_OPENCLAW_PILOT_AGENT_SEC_CLI",
    name: "agent-sec-cli",
    cliArg: agentSecCliBin,
    agentSecCliProject,
    pluginRoot,
    repoRoot,
  });
  const agentSecDaemon = resolveAgentSecExecutable({
    envName: "AGENT_SEC_OPENCLAW_PILOT_AGENT_SEC_DAEMON",
    name: "agent-sec-daemon",
    cliArg: agentSecDaemonBin,
    agentSecCliProject,
    pluginRoot,
    repoRoot,
  });
  const openclawWrapper = `#!/usr/bin/env bash
set -euo pipefail

OPENCLAW_BIN=${shellQuote(openclawBin)}
DEFAULT_OPENCLAW_LOG=${shellQuote(openclawCallsLog ?? "")}
LOG_FILE="\${AGENT_SEC_OPENCLAW_PILOT_OPENCLAW_LOG:-$DEFAULT_OPENCLAW_LOG}"

if [[ -n "$LOG_FILE" ]]; then
  OPENCLAW_BIN="$OPENCLAW_BIN" OPENCLAW_LOG_FILE="$LOG_FILE" node -e '
const { appendFileSync, mkdirSync } = require("node:fs");
const { dirname } = require("node:path");

try {
  const logFile = process.env.OPENCLAW_LOG_FILE;
  if (logFile) {
    mkdirSync(dirname(logFile), { recursive: true });
    appendFileSync(logFile, JSON.stringify({
      ts: new Date().toISOString(),
      command: process.env.OPENCLAW_BIN,
      args: process.argv.slice(1),
    }) + "\\n");
  }
} catch {
  // OpenClaw argv logging is evidence-only; never change command behavior.
}
' "$@" || true
fi

if [[ "$OPENCLAW_BIN" == *.mjs ]]; then
  exec node "$OPENCLAW_BIN" "$@"
fi
exec "$OPENCLAW_BIN" "$@"
`;
  const agentSecCliWrapper = buildAgentSecWrapper(agentSecCli, "agent-sec-cli");
  const agentSecDaemonWrapper = buildAgentSecWrapper(agentSecDaemon, "agent-sec-daemon");
  await writeExecutable(path.join(binDir, "openclaw"), openclawWrapper);
  await writeExecutable(path.join(binDir, "agent-sec-cli"), agentSecCliWrapper);
  await writeExecutable(path.join(binDir, "agent-sec-daemon"), agentSecDaemonWrapper);
  return {
    agentSecCli,
    agentSecDaemon,
  };
}

export function buildAgentSecCliOverrideConfig() {
  // Only the policy-matrix marker inputs are overridden. All other CLI calls go
  // to the real agent-sec-cli so smoke coverage still exercises the installed binary.
  return {
    "scan-prompt": [
      {
        inputIncludes: POLICY_PROMPT_DENY_MARKER,
        exitCode: 0,
        stdout: {
          verdict: "deny",
          threat_type: "prompt_injection",
          risk_level: "high",
          confidence: 0.99,
          findings: [
            {
              rule_id: "pilot-prompt-deny",
              desc_zh: "策略矩阵测试：提示词注入",
              desc_en: "Policy matrix test prompt injection",
            },
          ],
        },
      },
    ],
    "scan-code": [
      {
        inputIncludes: POLICY_CODE_DENY_COMMAND,
        exitCode: 0,
        stdout: {
          verdict: "deny",
          findings: [
            {
              rule_id: "pilot-code-deny",
              desc_zh: "策略矩阵测试：危险命令",
              desc_en: "Policy matrix test dangerous command",
            },
          ],
        },
      },
    ],
  };
}

function resolveAgentSecExecutable({
  agentSecCliProject,
  cliArg,
  envName,
  name,
  pluginRoot,
  repoRoot,
}) {
  if (cliArg) {
    return { kind: "binary", source: "cli-arg", command: path.resolve(cliArg) };
  }

  const envValue = process.env[envName];
  if (envValue) {
    return { kind: "binary", source: envName, command: envValue };
  }

  for (const candidate of agentSecVenvCandidates({ agentSecCliProject, name, pluginRoot, repoRoot })) {
    if (existsSync(candidate.path)) {
      return { kind: "binary", source: candidate.source, command: candidate.path };
    }
  }

  const hostPath = findExecutableOnPath(name, process.env.PATH ?? "");
  if (hostPath) {
    return { kind: "binary", source: "PATH", command: hostPath };
  }

  // uv-run is a last resort for developer machines that have not activated a
  // venv. CI and the current task normally use the repo-root .venv binary path.
  return {
    kind: "uv-run",
    source: "fallback",
    project: agentSecCliProject,
    command: name,
  };
}

function agentSecVenvCandidates({ agentSecCliProject, name, pluginRoot, repoRoot }) {
  return [
    { source: "repo-root-venv", path: path.join(repoRoot, ".venv", "bin", name) },
    { source: "cwd-venv", path: path.join(process.cwd(), ".venv", "bin", name) },
    { source: "agent-sec-cli-project-venv", path: path.join(agentSecCliProject, ".venv", "bin", name) },
    { source: "plugin-root-venv", path: path.join(pluginRoot, ".venv", "bin", name) },
  ];
}

function findExecutableOnPath(name, pathValue) {
  for (const entry of pathValue.split(path.delimiter)) {
    if (!entry) continue;
    const candidate = path.join(entry, name);
    if (existsSync(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function buildAgentSecWrapper(target, commandName) {
  if (commandName === "agent-sec-cli") {
    return buildAgentSecCliWrapper(target, commandName);
  }
  if (target.kind === "binary") {
    return `#!/usr/bin/env bash
set -euo pipefail
exec ${shellQuote(target.command)} "$@"
`;
  }
  return `#!/usr/bin/env bash
set -euo pipefail
exec uv run --project ${shellQuote(target.project)} ${shellQuote(commandName)} "$@"
`;
}

function buildAgentSecCliWrapper(target, commandName) {
  const commandSpec =
    target.kind === "binary"
      ? { command: target.command, prefixArgs: [] }
      : { command: "uv", prefixArgs: ["run", "--project", target.project, commandName] };
  return `#!/usr/bin/env node
const { spawnSync } = require("node:child_process");
const { appendFileSync, mkdirSync, readFileSync } = require("node:fs");
const { dirname } = require("node:path");

const command = ${JSON.stringify(commandSpec.command)};
const prefixArgs = ${JSON.stringify(commandSpec.prefixArgs)};
const args = process.argv.slice(2);

function parseInvocation(argv) {
  const offset = argv[0] === "--trace-context" ? 2 : 0;
  const subcommand = argv[offset];
  const inputFlag = subcommand === "scan-prompt" ? "--text" : subcommand === "scan-code" ? "--code" : undefined;
  const inputIndex = inputFlag ? argv.indexOf(inputFlag, offset + 1) : -1;
  return {
    offset,
    subcommand,
    input: inputIndex >= 0 && inputIndex + 1 < argv.length ? String(argv[inputIndex + 1]) : undefined,
  };
}

function readOverrideConfig() {
  const file = process.env.AGENT_SEC_OPENCLAW_PILOT_CLI_OVERRIDE_FILE;
  if (!file) return {};
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch {
    return {};
  }
}

function resolveOverride(invocation) {
  // Matching by input substring keeps the override independent of CLI argument
  // ordering while still proving the plugin supplied the expected scan input.
  const config = readOverrideConfig();
  const candidates = Array.isArray(config[invocation.subcommand]) ? config[invocation.subcommand] : [];
  for (const candidate of candidates) {
    if (
      typeof candidate?.inputIncludes === "string" &&
      typeof invocation.input === "string" &&
      invocation.input.includes(candidate.inputIncludes)
    ) {
      return candidate;
    }
  }
  return undefined;
}

function tryParseJson(text) {
  try {
    return JSON.parse(String(text).trim());
  } catch {
    return undefined;
  }
}

function writeCallLog(entry) {
  const file = process.env.AGENT_SEC_OPENCLAW_PILOT_CLI_LOG;
  if (!file) return;
  try {
    mkdirSync(dirname(file), { recursive: true });
    appendFileSync(file, JSON.stringify({
      ts: new Date().toISOString(),
      ...entry,
    }) + "\\n");
  } catch {
    // The wrapper must never break agent-sec-cli behavior because evidence logging failed.
  }
}

const invocation = parseInvocation(args);

// Capture synthetic prompt/PII input once for the evidence log and forward the
// identical bytes to the real CLI. Only scan-prompt has marker-based overrides.
let stdinBuffer;
if (invocation.input === undefined && (
  invocation.subcommand === "scan-prompt" ||
  (invocation.subcommand === "scan-pii" && args.includes("--stdin"))
)) {
  try {
    stdinBuffer = readFileSync(0, "utf8");
    invocation.input = stdinBuffer.length > 0 ? stdinBuffer : undefined;
  } catch {
    // stdin unavailable (e.g. tty); leave input undefined and fail-open.
  }
}

const override = resolveOverride(invocation);
if (override) {
  // Deterministic deny results are necessary for matrix acceptance. The wrapper
  // also records the call so the E2E can prove the plugin invoked agent-sec-cli.
  const stdout = JSON.stringify(override.stdout ?? {}) + "\\n";
  const stderr = typeof override.stderr === "string" ? override.stderr : "";
  const exitCode = Number.isInteger(override.exitCode) ? override.exitCode : 0;
  process.stdout.write(stdout);
  process.stderr.write(stderr);
  writeCallLog({
    args,
    subcommand: invocation.subcommand,
    input: invocation.input,
    override: true,
    exitCode,
    stdoutJson: tryParseJson(stdout),
    stderr,
  });
  process.exit(exitCode);
}

const stdinInput = stdinBuffer !== undefined ? stdinBuffer : (args.includes("--stdin") ? readFileSync(0) : undefined);
const child = spawnSync(command, [...prefixArgs, ...args], {
  encoding: "utf8",
  env: process.env,
  input: stdinInput,
});
const stdout = child.stdout ?? "";
const stderr = child.stderr ?? "";
process.stdout.write(stdout);
process.stderr.write(stderr);
const exitCode = child.status ?? (child.signal ? 1 : 0);
writeCallLog({
  args,
  subcommand: invocation.subcommand,
  input: invocation.input,
  override: false,
  exitCode,
  signal: child.signal ?? undefined,
  stdinBytes: stdinInput ? Buffer.byteLength(stdinInput) : 0,
  stdoutJson: tryParseJson(stdout),
  stdout: stdout.slice(0, 2000),
  stderr: stderr.slice(0, 2000),
});
process.exit(exitCode);
`;
}
