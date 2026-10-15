import { resolve, dirname, basename } from "node:path";
import { homedir } from "node:os";
import type { SecurityCapability } from "../types.js";
import {
  buildTraceContext,
  callAgentSecCli,
  envFlagEnabled,
  envHookPolicy,
  isHookPolicyValue,
  normalizeHookPolicy,
  truncateApprovalText,
  type HookPolicy,
  type TraceContext,
} from "../utils.js";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

type ExposureSummary = {
  latestStatus: string;
  message: string | null;
  skillName?: string;
  [key: string]: unknown;
};

type SkillLedgerPolicy = HookPolicy;

type SkillLedgerConfig = {
  policy: SkillLedgerPolicy;
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const READ_TOOL_NAMES = ["read"];
const PATH_PARAM_NAMES = ["file_path", "path"];
const DEFAULT_TIMEOUT_MS = 5_000;
const DEFAULT_POLICY: SkillLedgerPolicy = "ask";

// ---------------------------------------------------------------------------
// Confirmation policy
// ---------------------------------------------------------------------------

const CONFIRMATION_SEVERITY: Record<string, "warning" | "critical"> = {
  warn: "warning",
  none: "warning",
  drifted: "warning",
  deny: "critical",
  tampered: "critical",
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function expandHomePath(filePath: string): string {
  if (filePath === "~") {
    return homedir();
  }
  if (filePath.startsWith("~/")) {
    return homedir() + filePath.slice(1);
  }
  return filePath;
}

/** Extract the file path from a before_tool_call event, or undefined if not a read-SKILL.md call. */
function extractSkillPath(
  event: { toolName: string; params: Record<string, unknown> },
): string | undefined {
  if (!READ_TOOL_NAMES.includes(event.toolName)) return undefined;

  let filePath: string | undefined;
  for (const paramName of PATH_PARAM_NAMES) {
    const val = event.params[paramName];
    if (typeof val === "string" && val.trim()) {
      filePath = val.trim();
      break;
    }
  }
  if (!filePath) return undefined;

  // Resolve to canonical absolute path to neutralize ".." traversal
  const resolved = resolve(expandHomePath(filePath));

  if (!resolved.endsWith("/SKILL.md")) return undefined;

  return resolved;
}

/** Resolve skill_dir from the matched SKILL.md path. */
function resolveSkillDir(skillMdPath: string): string {
  return resolve(dirname(skillMdPath));
}

function confirmationSeverity(status: string): "warning" | "critical" | undefined {
  return CONFIRMATION_SEVERITY[status];
}

function readPolicy(
  capabilityConfig: Record<string, any>,
  api: any,
): SkillLedgerPolicy {
  if (process.env.SKILL_LEDGER_MODE !== undefined) {
    const rawPolicy = process.env.SKILL_LEDGER_MODE;
    const policy = envHookPolicy("SKILL_LEDGER_MODE", DEFAULT_POLICY);
    if (!isHookPolicyValue(rawPolicy)) {
      api.logger.warn("[skill-ledger] invalid SKILL_LEDGER_MODE; using ask");
    }
    return policy;
  }

  if (typeof capabilityConfig.policy === "string") {
    if (isHookPolicyValue(capabilityConfig.policy)) {
      return normalizeHookPolicy(capabilityConfig.policy, DEFAULT_POLICY);
    }
    api.logger.warn(
      `[skill-ledger] invalid policy="${capabilityConfig.policy}"; using ${DEFAULT_POLICY}`,
    );
    return DEFAULT_POLICY;
  }

  if (typeof capabilityConfig.enableBlock === "boolean") {
    return capabilityConfig.enableBlock ? "block" : "warn";
  }

  return DEFAULT_POLICY;
}

function readConfig(pluginConfig: Record<string, any>, api: any): SkillLedgerConfig {
  const capabilityConfig = pluginConfig.capabilities?.["skill-ledger"] ?? {};
  return {
    policy: readPolicy(capabilityConfig, api),
  };
}

function logDebug(api: any, message: string): void {
  api.logger.debug?.(`[skill-ledger] ${message}`);
}

function logDiagnostic(api: any, cfg: SkillLedgerConfig, message: string): void {
  if (cfg.policy === "observe") {
    logDebug(api, message);
  } else {
    api.logger.warn(`[skill-ledger] ${message}`);
  }
}

// ---------------------------------------------------------------------------
// Capability
// ---------------------------------------------------------------------------

export const skillLedger: SecurityCapability = {
  id: "skill-ledger",
  name: "Skill Ledger",
  hooks: ["before_tool_call"],
  register(api) {
    if (!envFlagEnabled("SKILL_LEDGER_HOOK_ENABLED", true)) {
      return;
    }
    const cfg = readConfig((api.pluginConfig as Record<string, any>) ?? {}, api);

    async function ensureKeys(traceContext?: TraceContext): Promise<boolean> {
      // The daemon owns readiness and serializes concurrent initialization.
      try {
        const result = await callAgentSecCli(
          ["skill-ledger", "init", "--no-baseline"],
          { timeout: DEFAULT_TIMEOUT_MS, traceContext },
        );
        if (result.exitCode === 0) return true;
        logDiagnostic(api, cfg, `init --no-baseline failed: exit ${result.exitCode}`);
      } catch {
        logDiagnostic(api, cfg, "init --no-baseline failed");
      }
      return false;
    }

    // ── Hook handlers ───────────────────────────────────────────────
    api.on(
      "before_tool_call",
      async (event: any, ctx: any) => {
        try {
          const skillMdPath = extractSkillPath(event);
          if (!skillMdPath) return undefined;

          const skillDir = resolveSkillDir(skillMdPath);
          const skillName = basename(skillDir);
          const traceContext = buildTraceContext(event, ctx);

          // Ensure keys are ready
          if (!(await ensureKeys(traceContext))) return undefined;

          // Invoke CLI
          const result = await callAgentSecCli(
            ["skill-ledger", "show", skillDir],
            { timeout: DEFAULT_TIMEOUT_MS, traceContext },
          );

          if (result.exitCode !== 0) {
            logDiagnostic(api, cfg, `show failed: exit ${result.exitCode}`);
            return undefined;
          }
          let summary: ExposureSummary;
          try {
            summary = JSON.parse(result.stdout) as ExposureSummary;
          } catch {
            logDiagnostic(api, cfg, "invalid show JSON");
            return undefined;
          }
          if (!summary || typeof summary !== "object" || Array.isArray(summary) || summary.status === "error") {
            logDiagnostic(api, cfg, "invalid show response");
            return undefined;
          }
          if (summary.managed === false) return undefined;
          if (
            (summary.managed !== undefined && summary.managed !== true) ||
            !["pass", "none", "drifted", "warn", "deny", "tampered"].includes(summary.latestStatus) ||
            !("message" in summary) ||
            (summary.message !== null && typeof summary.message !== "string")
          ) {
            logDiagnostic(api, cfg, "invalid show summary");
            return undefined;
          }

          if (typeof summary.message !== "string" || !summary.message.trim()) {
            return undefined;
          }

          const status = summary.latestStatus ?? "unknown";
          const message = `⚠️ Skill '${skillName}': ${summary.message}`;
          if (cfg.policy === "observe") {
            logDebug(api, `skill='${skillName}' status=${status}: ${message}`);
            return undefined;
          }

          api.logger.warn(`[skill-ledger] ${message}`);
          if (cfg.policy === "block") {
            return { block: true, blockReason: message };
          }

          const severity = confirmationSeverity(status);
          if (cfg.policy === "ask" && severity) {
            const details = "\nDetails: logs.";
            const description = truncateApprovalText(
              `[skill-ledger][${status}] Skill: ${truncateApprovalText(skillName, 48)}; ${summary.message}`,
              256 - details.length,
            ) + details;
            return {
              requireApproval: {
                title: "Skill Ledger Security Check",
                description,
                severity,
              },
            };
          }

          // For warn/error/unknown states, log and allow. Fail-open behavior for
          // CLI/runtime failures remains handled by the catch/parse branches.
          return undefined;
        } catch {
          // Fail-open: uncaught errors must never block tool calls
          logDiagnostic(api, cfg, "Skill check failed before a valid response");
          return undefined;
        }
      },
      { priority: 80 },
    );
  },
};
