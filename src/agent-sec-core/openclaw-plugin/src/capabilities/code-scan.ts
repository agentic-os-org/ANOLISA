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
      try {
        if (!hookEnabled) {
          return undefined;
        }

        // 只拦截 shell 类工具
        const command = extractCommand(event);
        if (!command) {
          return undefined;
        }

        const result = await callAgentSecCli(
          ["scan-code", "--code", command, "--language", "bash"],
          { timeout: 10000, traceContext: buildTraceContext(event, ctx) },
        );

        if (result.exitCode !== 0) {
          return undefined;
        }

        const scanResult = JSON.parse(result.stdout);
        const verdict = scanResult.verdict;
        const findings = scanResult.findings ?? [];

        // Self-protect: force block if the command would disable this plugin
        const selfProtectFinding = findings.find(
          (f: any) => f.rule_id === "shell-self-protect-openclaw",
        );
        if (selfProtectFinding) {
          const msg = `[agent-sec-core] 自我保护：该命令将禁用 agent-sec 安全插件。如果您确实需要禁用，请手动执行以下命令：\n\n  ${command}\n\n出于安全原因，AI agent 无法执行此操作。`;
          api.logger.warn(`[scan-code] SELF-PROTECT block — ${command}`);
          return { block: true, blockReason: msg };
        }

        if (verdict === "pass" || findings.length === 0) {
          api.logger.info(`[scan-code] ✅ pass — allowing command`);
          return undefined;
        }

        // 构建提示信息（与 cosh hook 的 msg 格式一致）
        const descs = findings.map((f: any) => `- ${f.desc_zh}`);
        const msg = `[code-scanner] Detected ${findings.length} issue(s):\n${descs.join("\n")}\n\nCommand: ${command}`;

        if (verdict === "deny") {
          api.logger.warn(
            `[scan-code] DENY (policy=${policy}) — ${msg}`,
          );
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
          api.logger.warn(`[scan-code] WARN (policy=${policy}) — ${msg}`);
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
