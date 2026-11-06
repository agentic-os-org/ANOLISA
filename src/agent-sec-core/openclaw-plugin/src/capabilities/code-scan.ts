import type { SecurityCapability } from "../types.js";
import {
  buildTraceContext,
  callAgentSecCli,
  envFlagEnabled,
  isHookPolicyValue,
  normalizeHookPolicy,
  truncateApprovalText,
} from "../utils.js";

/** Tools that can disable or remove a plugin without running a shell command. */
const PLUGIN_ADMIN_TOOLS = new Set(["plugins", "tool_call", "openclaw"]);
/** Actions that would remove agent-sec's protection from the gateway. */
const SELF_PROTECT_ACTIONS = new Set(["disable", "uninstall"]);
/** Plugin ids that carry agent-sec's protections. */
const SELF_PROTECT_TARGETS = new Set(["agent-sec", "agent-sec-core", "sec-core", "agent-security"]);
/** `agent-sec` as a whole token, e.g. inside `global:agent-sec/dist/index.js`. */
const SELF_PROTECT_TARGET_TOKEN = /(^|[^a-z0-9_-])agent-sec([^a-z0-9_-]|$)/;

type PluginMutation = { action: string; target: string };

function firstString(...values: unknown[]): string | undefined {
  return values.find((value): value is string => typeof value === "string");
}

/**
 * Match the plugin ids that carry agent-sec, ignoring a version, scope or
 * resolved path: `agent-sec`, `agent-sec@1.1.2`, `global:agent-sec/dist/x.js`.
 */
function isSelfProtectTarget(target: string): boolean {
  const normalized = target.trim().toLowerCase();
  const lastSegment = normalized.split("/").pop() ?? normalized;
  const name = (lastSegment.split("@")[0] ?? lastSegment).split(":").pop() ?? lastSegment;
  return SELF_PROTECT_TARGETS.has(name) || SELF_PROTECT_TARGET_TOKEN.test(normalized);
}

/**
 * Detect a plugin disable or uninstall issued through a native admin tool.
 *
 * The gateway dispatches `before_tool_call` for the `plugins` tool with the
 * action in `params`, and for the `tool_call` wrapper with the same fields one
 * level down in `args`; both the `pluginId` and `name` spellings are accepted
 * because the model uses them interchangeably. These arguments are not shell
 * text, so scan-code has nothing to inspect and the block must happen here.
 */
function extractPluginMutation(event: {
  toolName: string;
  params?: Record<string, unknown>;
}): PluginMutation | undefined {
  if (!PLUGIN_ADMIN_TOOLS.has(event.toolName)) return undefined;
  const params = (event.params ?? {}) as Record<string, unknown>;
  const args = (params.args && typeof params.args === "object" ? params.args : {}) as Record<
    string,
    unknown
  >;
  const action = firstString(params.action, args.action);
  const target = firstString(
    params.pluginId,
    params.packageName,
    params.name,
    args.pluginId,
    args.packageName,
    args.name,
  );
  if (action === undefined || !SELF_PROTECT_ACTIONS.has(action.toLowerCase())) return undefined;
  if (target === undefined || !isSelfProtectTarget(target)) return undefined;
  return { action: action.toLowerCase(), target };
}

export const codeScan: SecurityCapability = {
  id: "scan-code",
  name: "Code Scanner",
  hooks: ["before_tool_call"],
  register(api) {
    const cfg = (api.pluginConfig as Record<string, any>) ?? {};
    const hookEnabled = envFlagEnabled("CODE_SCANNER_HOOK_ENABLED", true);
    const fallbackPolicy = cfg.codeScanRequireApproval === true ? "ask" : "observe";
    const rawPolicy = process.env.CODE_SCANNER_MODE;
    const configuredPolicy = normalizeHookPolicy(rawPolicy, fallbackPolicy);
    const policy =
      configuredPolicy === "observe" || configuredPolicy === "ask" || configuredPolicy === "block"
        ? configuredPolicy
        : fallbackPolicy;
    if (
      rawPolicy !== undefined &&
      (!isHookPolicyValue(rawPolicy) || configuredPolicy === "warn")
    ) {
      api.logger.warn(
        `[scan-code] invalid or unsupported CODE_SCANNER_MODE=${JSON.stringify(rawPolicy.slice(0, 32))}; using ${policy}`,
      );
    }

    api.on("before_tool_call", async (event: any, ctx: any) => {
      const startedAt = performance.now();
      const traceContext = buildTraceContext(event, ctx);
      let failureReason = "cli-error";
      const report = (level: "info" | "warn", message: string, details: Record<string, unknown>) => {
        api.logger[level](
          `[scan-code] ${JSON.stringify({
            schemaVersion: 1,
            ...traceContext,
            policy,
            ...details,
            message,
          })}`,
        );
      };
      try {
        if (!hookEnabled) {
          return undefined;
        }

        // Self-protect first: the native plugin tools carry no shell text, so
        // the scanner below never sees an attempt to disable this plugin.
        const mutation = extractPluginMutation(event);
        if (mutation) {
          const msg = `[agent-sec-core] 自我保护：该操作将禁用 agent-sec 安全插件（${mutation.action} ${mutation.target}）。出于安全原因，AI agent 无法执行此操作。`;
          report("warn", `SELF-PROTECT block (${event.toolName}) — ${mutation.action} ${mutation.target}`, {
            outcome: "self-protect-native",
            decision: "block",
          });
          return { block: true, blockReason: msg };
        }

        // 只拦截 shell 类工具
        const command = extractCommand(event);
        if (!command) {
          return undefined;
        }

        const result = await callAgentSecCli(
          ["scan-code", "--code", command, "--language", "bash"],
          { timeout: 10000, traceContext },
        );

        if (result.exitCode !== 0) {
          report("warn", "scanner failed; allowing execution", { outcome: "scanner-failed", reason: failureReason, exitCode: result.exitCode, decision: "allow" });
          return undefined;
        }

        failureReason = "invalid-response";
        const scanResult = JSON.parse(result.stdout);
        if (!scanResult || !["pass", "warn", "deny", "error"].includes(scanResult.verdict) ||
            (scanResult.findings !== undefined && !Array.isArray(scanResult.findings))) {
          throw new SyntaxError("Invalid scanner response");
        }
        if (scanResult.verdict === "error") {
          report("warn", "scanner failed; allowing execution", { outcome: "scanner-failed", reason: "scanner-error", decision: "allow" });
          return undefined;
        }
        failureReason = "decision-error";
        const verdict = scanResult.verdict;
        const findings = scanResult.findings ?? [];

        // Self-protect: force block if the command would disable this plugin
        const selfProtectFinding = findings.find(
          (f: any) => f.rule_id === "shell-self-protect-openclaw",
        );
        if (selfProtectFinding) {
          const msg = `[agent-sec-core] 自我保护：该命令将禁用 agent-sec 安全插件。如果您确实需要禁用，请手动执行以下命令：\n\n  ${command}\n\n出于安全原因，AI agent 无法执行此操作。`;
          report("warn", `SELF-PROTECT block — ${command}`, { outcome: "scan-result", verdict, decision: "block" });
          return { block: true, blockReason: msg };
        }

        if (verdict === "pass" || findings.length === 0) {
          report("info", "✅ pass — allowing command", { outcome: "scan-result", verdict, decision: "allow" });
          return undefined;
        }

        // 构建提示信息（与 cosh hook 的 msg 格式一致）
        const descs = findings.map((f: any) => `- ${f.desc_zh}`);
        const msg = `[code-scanner] Detected ${findings.length} issue(s):\n${descs.join("\n")}\n\nCommand: ${command}`;
        const details = "\nDetails: logs.";
        const description = truncateApprovalText(
          `[code-scanner][${verdict}] ${findings.length} issue(s); Command: ${truncateApprovalText(command, 64)}; ${descs.join("\n")}`,
          256 - details.length,
        ) + details;

        if (verdict === "deny") {
          report("warn", `DENY (policy=${policy}) — ${msg}`, {
            outcome: "scan-result",
            verdict,
            decision:
              policy === "ask"
                ? "requireApproval"
                : policy === "block"
                  ? "block"
                  : "allow",
          });
          if (policy === "block") {
            return { block: true, blockReason: msg };
          }
          if (policy === "ask") {
            return {
              requireApproval: {
                title: "Code Scanner Security Warning",
                description,
                severity: "warning" as const,
              },
            };
          }
          return undefined;
        }

        if (verdict === "warn") {
          report("warn", `WARN (policy=${policy}) — ${msg}`, {
            outcome: "scan-result",
            verdict,
            decision:
              policy === "ask"
                ? "requireApproval"
                : policy === "block"
                  ? "block"
                  : "allow",
          });
          if (policy === "block") {
            return { block: true, blockReason: msg };
          }
          if (policy === "ask") {
            return {
              requireApproval: {
                title: "Code Scanner Security Warning",
                description,
                severity: "warning" as const,
              },
            };
          }
          return undefined;
        }

        return undefined;
      } catch (err) {
        report("warn", "hook failed; allowing execution", {
          outcome: failureReason === "decision-error" ? "hook-error" : "scanner-failed",
          reason: failureReason,
          errorType: err instanceof Error ? err.name : typeof err,
          elapsedMs: Math.round(performance.now() - startedAt),
          decision: "allow",
        });
        return undefined; // crash ≠ threat → allow
      }
    });
  },
};

/** 从 event 中提取 shell 命令，无法提取则返回 undefined */
function extractCommand(event: { toolName: string; params: Record<string, unknown> }): string | undefined {
  // OpenClaw 唯一的 shell 执行工具是 exec，参数字段为 command
  // 参考: https://docs.openclaw.ai/tools/exec
  if (event.toolName !== "exec") return undefined;
  const cmd = event.params.command;
  if (typeof cmd !== "string" || !cmd.trim()) return undefined;
  return cmd;
}
