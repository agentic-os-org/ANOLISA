import type { SecurityCapability } from "../types.js";
import { inboundPiiScanText } from "../helpers/pii-text.js";
import { buildTraceContext, callAgentSecCli, envFlagEnabled } from "../utils.js";

/**
 * 用户输入 Prompt 注入 / 越狱检测。
 *
 * ## 当前防线：before_dispatch (priority 190)
 * 在用户消息进入系统的最早时机拦截，此时 event 只含用户原始输入，
 * 不包含工具输出或 RAG 内容，因此只覆盖用户侧的直接注入 / 越狱攻击。
 *
 * ## 后续待补：before_prompt_build（第二道防线）
 * prompt 组装完成后（用户输入 + 工具输出 + RAG 上下文全部拼入），
 * 再做一次 scan-prompt，可覆盖间接注入（tool output 投毒、RAG 投毒等）。
 * 届时独立为新能力 id="prompt-scan-full"，挂 before_prompt_build。
 *
 * Scan mode (controlled by PROMPT_SCANNER_SCAN_MODE env var, default: standard):
 *   - fast: lightweight heuristics, lower latency.
 *   - standard: balanced detection (default).
 *   - strict: not implemented yet; currently behaves the same as standard.
 * CLI: agent-sec-cli scan-prompt --mode <fast|standard|strict> --format json --source user_input
 *      (prompt text is piped via stdin to avoid /proc/cmdline leak & ARG_MAX)
 */
export const promptScan: SecurityCapability = {
  id: "prompt-scan",
  name: "Prompt Injection Scanner",
  hooks: ["before_dispatch"],
  register(api) {
    if (!envFlagEnabled("PROMPT_SCANNER_HOOK_ENABLED", true)) {
      return;
    }
    const cfg = (api.pluginConfig as Record<string, any>) ?? {};
    api.on("before_dispatch", async (event: any, ctx: any) => {
      try {
        const text = inboundPiiScanText(event);
        if (!text.trim()) {
          return undefined;
        }

        const rawScanMode = (process.env.PROMPT_SCANNER_SCAN_MODE ?? "standard").trim().toLowerCase();
        const validScanMode = ["fast", "standard", "strict"].includes(rawScanMode) ? rawScanMode : "standard";
        api.logger.info(
          `[prompt-scan] scan mode configured: raw=${JSON.stringify(rawScanMode)}, effective=${JSON.stringify(validScanMode)}`,
        );
        // Pipe prompt via stdin (not --text argv) to avoid /proc/<pid>/cmdline
        // exposure and ARG_MAX limits — mirrors codex/hermes/qoder/qwen.
        const result = await callAgentSecCli(
          ["scan-prompt", "--mode", validScanMode, "--format", "json", "--source", "user_input"],
          { timeout: 10000, stdin: text, traceContext: buildTraceContext(event, ctx) },
        );

        if (result.exitCode !== 0) {
          return undefined; // CLI 不可用 -> fail-open
        }

        const scanResult = JSON.parse(result.stdout);
        const verdict = scanResult.verdict;
        const findings: any[] = scanResult.findings ?? [];

        if (verdict === "pass" || findings.length === 0) {
          api.logger.info(`[prompt-scan] pass`);
          return undefined;
        }

        const threatType: string = scanResult.threat_type ?? "";
        const riskLevel: string = scanResult.risk_level ?? "unknown";
        const confidence: number | undefined = scanResult.confidence;

        const detailLines: string[] = [
          `  攻击类型 : ${threatType || "unknown"}`,
          `  风险等级 : ${riskLevel}`,
          `  拦截环节 : 用户输入扫描 (before_dispatch)`,
          ...(confidence != null ? [`  模型置信度: ${(confidence * 100).toFixed(1)}%`] : []),
        ];
        const detailMsg = detailLines.join("\n");

        if (verdict === "deny") {
          const text = `[prompt-scan] 检测到安全风险\n${detailMsg}`;
          api.logger.warn(text);
          // handled: true + text → text sent as final reply, LLM call skipped
          // handled: false + text → text ignored, event passes through to LLM
          // promptScanBlock=true (openclaw.json) 开启拦截模式
          api.logger.warn(`[prompt-scan] promptScanBlock=${cfg.promptScanBlock}`);
          const blockEnabled = cfg.promptScanBlock === true;
          return { handled: blockEnabled, text };
        }

        if (verdict === "warn") {
          api.logger.warn(`[prompt-scan] WARN — passing user prompt with warning`);
          return { handled: false, text: `[Security Warning] ${detailMsg}` };
        }

        return undefined;
      } catch {
        return undefined; // crash ≠ threat -> fail-open
      }
    }, { priority: 190 });
  },
};
