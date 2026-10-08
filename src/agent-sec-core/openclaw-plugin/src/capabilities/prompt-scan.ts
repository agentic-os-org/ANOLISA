import type { SecurityCapability } from "../types.js";
import {
  inboundPiiScanText,
  modelInputPiiScanText,
} from "../helpers/pii-text.js";
import {
  buildTraceContext,
  callAgentSecCli,
  envFlagEnabled,
  supportsModelInputGate,
} from "../utils.js";

/**
 * 用户输入 Prompt 注入 / 越狱检测。
 *
 * ## 当前防线：model-entry gate (priority 190)
 * Stable OpenClaw >=2026.5.12 把入站文本从 before_dispatch 挪到
 * before_agent_run，因此新主机在 before_agent_run 扫描组装后的模型输入
 * （systemPrompt + prompt + 消息文本）；老主机（含 prerelease）仍走
 * before_dispatch legacy 入站事件。两条路径都只覆盖用户侧的直接注入 /
 * 越狱攻击，不包含工具输出或 RAG 内容。
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
 * CLI: agent-sec-cli scan-prompt --mode <fast|standard|strict> --format json
 *      (prompt text is piped via stdin to avoid /proc/cmdline leak & ARG_MAX)
 */
type ScanOutcome = {
  verdict: "deny" | "warn";
  message: string;
};

export const promptScan: SecurityCapability = {
  id: "prompt-scan",
  name: "Prompt Injection Scanner",
  hooks: ["before_agent_run", "before_dispatch"],
  register(api) {
    if (!envFlagEnabled("PROMPT_SCANNER_HOOK_ENABLED", true)) {
      return;
    }
    const cfg = (api.pluginConfig as Record<string, any>) ?? {};
    const modelInput = supportsModelInputGate(api.runtime?.version);
    const inputHook = modelInput ? "before_agent_run" : "before_dispatch";
    api.logger.info(`[prompt-scan] input hook: ${inputHook}`);
    if (!modelInput) {
      api.logger.warn(
        "[prompt-scan] using legacy inbound scanning; model-input scanning requires a stable OpenClaw >=2026.5.12",
      );
    }
    // promptScanBlock=true (openclaw.json) 开启拦截模式
    const blockEnabled = cfg.promptScanBlock === true;
    const scanInput = async (
      event: any,
      ctx: any,
    ): Promise<ScanOutcome | undefined> => {
      try {
        const text = modelInput
          ? modelInputPiiScanText(event)
          : inboundPiiScanText(event);
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
          ["scan-prompt", "--mode", validScanMode, "--format", "json", "--source", modelInput ? "model_input" : "user_input"],
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
          `  拦截环节 : 用户输入扫描 (${inputHook})`,
          ...(confidence != null ? [`  模型置信度: ${(confidence * 100).toFixed(1)}%`] : []),
        ];
        const detailMsg = detailLines.join("\n");

        if (verdict === "deny") {
          const warnText = `[prompt-scan] 检测到安全风险\n${detailMsg}`;
          api.logger.warn(warnText);
          api.logger.warn(`[prompt-scan] promptScanBlock=${cfg.promptScanBlock}`);
          return { verdict: "deny", message: warnText };
        }

        if (verdict === "warn") {
          api.logger.warn(`[prompt-scan] WARN — passing user prompt with warning`);
          return { verdict: "warn", message: detailMsg };
        }

        return undefined;
      } catch {
        return undefined; // crash ≠ threat -> fail-open
      }
    };
    if (modelInput) {
      api.on(
        "before_agent_run",
        async (event: any, ctx: any) => {
          const outcome = await scanInput(event, ctx);
          // The model-entry gate only understands {outcome: "block"}; without
          // promptScanBlock a deny verdict is audit-logged only (fail-open),
          // and a warn verdict is audit-only (mirrors pii-scan).
          return outcome?.verdict === "deny" && blockEnabled
            ? {
                outcome: "block",
                reason: "prompt_scan_deny",
                message: outcome.message,
              }
            : undefined;
        },
        { priority: 190 },
      );
    } else {
      api.on(
        "before_dispatch",
        async (event: any, ctx: any) => {
          const outcome = await scanInput(event, ctx);
          // handled: true + text → text sent as final reply, LLM call skipped
          // handled: false + text → text ignored, event passes through to LLM
          if (outcome?.verdict === "deny") {
            return { handled: blockEnabled, text: outcome.message };
          }
          if (outcome?.verdict === "warn") {
            return {
              handled: false,
              text: `[Security Warning] ${outcome.message}`,
            };
          }
          return undefined;
        },
        { priority: 190 },
      );
    }
  },
};
