import { accessSync, constants as fsConstants } from "node:fs";
import path from "node:path";

// Keep CLI parsing separate from the pilot orchestration so the top-level test
// file only describes the acceptance flow.
export function parseArgs(argv) {
  const parsed = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--help" || arg === "-h") {
      parsed.help = true;
    } else if (arg === "--pii-only") {
      parsed.piiOnly = true;
    } else if (arg === "--skip-gateway") {
      parsed.skipGateway = true;
    } else if (arg === "--skip-failure-probes") {
      parsed.skipFailureProbes = true;
    } else if (arg === "--workdir") {
      parsed.workdir = argv[++index];
    } else if (arg === "--openclaw-bin") {
      parsed.openclawBin = argv[++index];
    } else if (arg === "--agent-sec-cli") {
      parsed.agentSecCli = argv[++index];
    } else if (arg === "--agent-sec-daemon") {
      parsed.agentSecDaemon = argv[++index];
    } else if (arg === "--plugin-package") {
      parsed.pluginPackage = argv[++index];
    } else if (arg === "--port") {
      parsed.port = argv[++index];
    } else if (arg === "--gateway-timeout-ms") {
      parsed.gatewayTimeoutMs = argv[++index];
    } else if (arg === "--gateway-token") {
      parsed.gatewayToken = argv[++index];
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }
  if (parsed.piiOnly && parsed.skipGateway) {
    throw new Error("--pii-only requires a live Gateway; it cannot be combined with --skip-gateway");
  }
  return parsed;
}

export function printHelp() {
  console.log(`Usage: npm run e2e:openclaw -- [options]

Options:
  --workdir <dir>              Keep all state, logs, and artifacts under dir.
  --openclaw-bin <path>        OpenClaw executable or openclaw.mjs path.
  --agent-sec-cli <path>       Installed agent-sec-cli binary.
  --agent-sec-daemon <path>    Installed agent-sec-daemon binary.
  --plugin-package <path>      Prebuilt agent-sec OpenClaw plugin .tgz.
  --port <port>                Gateway port. Defaults to a free local port.
  --gateway-timeout-ms <ms>    Gateway health wait budget.
  --gateway-token <token>      Gateway token for local health checks.
  --pii-only                   Run only the PII Gateway regression lane.
  --skip-gateway               Install and inspect without starting gateway.
  --skip-failure-probes        Skip negative hook probes.
`);
}

export function resolveOpenClawBin(cliArg) {
  if (cliArg) return resolveExecutableReference(cliArg) ?? path.resolve(cliArg);
  if (process.env.OPENCLAW_BIN) {
    const resolved = resolveExecutableReference(process.env.OPENCLAW_BIN);
    if (resolved) return resolved;
    throw new Error(`Unable to find OpenClaw executable from OPENCLAW_BIN=${process.env.OPENCLAW_BIN}`);
  }
  const openclaw = resolveExecutableFromPath("openclaw");
  if (!openclaw) {
    throw new Error("Unable to find OpenClaw executable. Install openclaw or pass --openclaw-bin/OPENCLAW_BIN.");
  }
  return openclaw;
}

function resolveExecutableReference(value) {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  if (trimmed.includes(path.sep) || path.isAbsolute(trimmed)) {
    return path.resolve(trimmed);
  }
  return resolveExecutableFromPath(trimmed);
}

function resolveExecutableFromPath(command) {
  for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, command);
    if (isExecutable(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function isExecutable(file) {
  try {
    accessSync(file, fsConstants.X_OK);
    return true;
  } catch {
    return false;
  }
}
