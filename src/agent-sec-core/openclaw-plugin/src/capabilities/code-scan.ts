import type { SecurityCapability } from "../types.js";
import {
  buildTraceContext,
  callAgentSecCli,
  envFlagEnabled,
  isHookPolicyValue,
  normalizeHookPolicy,
} from "../utils.js";

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

        const selfProtectOperation = extractSelfProtectOperation(event);
        if (selfProtectOperation) {
          return blockSelfProtect(api, selfProtectOperation);
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
          report("warn", `SELF-PROTECT block — ${command}`, {
            outcome: "scan-result",
            verdict,
            decision: "block",
          });
          return blockSelfProtect(api, command);
        }

        if (verdict === "pass" || findings.length === 0) {
          report("info", "✅ pass — allowing command", { outcome: "scan-result", verdict, decision: "allow" });
          return undefined;
        }

        // 构建提示信息（与 cosh hook 的 msg 格式一致）
        const descs = findings.map((f: any) => `- ${f.desc_zh}`);
        const msg = `[code-scanner] Detected ${findings.length} issue(s):\n${descs.join("\n")}\n\nCommand: ${command}`;

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
                description: msg,
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
                description: msg,
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

/** Extract text passed to a tool boundary that can remove this plugin. */
function extractCommand(event: { toolName: string; params: Record<string, unknown> }): string | undefined {
  if (event.toolName === "exec") {
    const command = event.params.command;
    return typeof command === "string" && command.trim() ? command : undefined;
  }
  if (event.toolName === "openclaw") {
    const message = event.params.message;
    return typeof message === "string" && message.trim() ? message : undefined;
  }
  return undefined;
}

function extractSelfProtectOperation(event: {
  toolName: string;
  params: Record<string, unknown>;
}): string | undefined {
  if (
    event.toolName === "openclaw" &&
    event.params.action === "plugin_uninstall" &&
    event.params.pluginId === "agent-sec"
  ) {
    return "openclaw plugin_uninstall agent-sec";
  }
  return undefined;
}

function blockSelfProtect(
  api: { logger: { warn(message: string): void } },
  operation: string,
): { block: true; blockReason: string } {
  const message = `[agent-sec-core] 自我保护：该操作将禁用或卸载 agent-sec 安全插件。如果您确认需要，请手动执行或完成该操作。\n\n操作：${operation}`;
  api.logger.warn(`[scan-code] SELF-PROTECT block — ${operation}`);
  return { block: true, blockReason: message };
}
