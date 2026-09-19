import type { SecurityCapability } from "../types.js";
import { afterToolCallPiiScanText, inboundPiiScanText, valueToText } from "../helpers/pii-text.js";
import {
  buildTraceContext,
  callAgentSecCli,
  envFlagEnabled,
  envHookPolicy,
  isHookPolicyValue,
  normalizeHookPolicy,
  type HookPolicy,
} from "../utils.js";

const CLI_TIMEOUT_MS = 10_000;
const BEFORE_DISPATCH_PRIORITY = 200;

type PiiScanConfig = {
  scanUserInput: boolean;
  includeLowConfidence: boolean;
  policy: HookPolicy;
};

function readConfig(pluginConfig: Record<string, any>, api: any): PiiScanConfig {
  const capabilityConfig = pluginConfig.capabilities?.["pii-scan-user-input"] ?? {};
  let policy: HookPolicy = "observe";
  if (process.env.PII_CHECKER_MODE !== undefined) {
    policy = envHookPolicy("PII_CHECKER_MODE", "observe");
    if (!isHookPolicyValue(process.env.PII_CHECKER_MODE)) {
      api.logger.warn("[pii-checker] invalid PII_CHECKER_MODE; using observe");
    }
  } else if (typeof capabilityConfig.policy === "string") {
    policy = normalizeHookPolicy(capabilityConfig.policy, "observe");
    if (!isHookPolicyValue(capabilityConfig.policy)) {
      api.logger.warn(
        `[pii-checker] invalid capability policy="${capabilityConfig.policy}"; using observe`,
      );
    }
  } else if (typeof capabilityConfig.enableBlock === "boolean") {
    policy = capabilityConfig.enableBlock ? "block" : "warn";
  }
  return {
    scanUserInput:
      process.env.PII_CHECKER_HOOK_ENABLED !== undefined
        ? envFlagEnabled("PII_CHECKER_HOOK_ENABLED", true)
        : pluginConfig.piiScanUserInput !== false,
    includeLowConfidence: pluginConfig.piiIncludeLowConfidence === true,
    policy,
  };
}

function safeString(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function findingRisk(finding: Record<string, unknown>, verdict: string): "high" | "general" {
  const severity = safeString(finding.severity);
  if (severity === "deny") {
    return "high";
  }
  if (severity === "warn") {
    return "general";
  }
  return verdict === "deny" ? "high" : "general";
}

function riskSummary(
  verdict: string,
  findings: Record<string, unknown>[],
  summary?: unknown,
): string {
  if (typeof summary === "object" && summary !== null) {
    const data = summary as Record<string, unknown>;
    if (
      data.findings_truncated === true &&
      typeof data.by_severity === "object" &&
      data.by_severity !== null
    ) {
      const counts = data.by_severity as Record<string, unknown>;
      const high = counts.deny ?? 0;
      const general = counts.warn ?? 0;
      const total = data.total;
      if (
        typeof high === "number" &&
        typeof general === "number" &&
        Number.isSafeInteger(high) &&
        Number.isSafeInteger(general) &&
        high >= 0 &&
        general >= 0 &&
        total === high + general
      ) {
        return `检测到 ${total} 项敏感信息（高风险 ${high}、一般风险 ${general}；明细已省略）`;
      }
    }
  }
  const highCount = findings.filter((finding) => findingRisk(finding, verdict) === "high").length;
  const generalCount = findings.length - highCount;

  if (highCount > 0 && generalCount > 0) {
    return `检测到 ${findings.length} 项敏感信息（高风险 ${highCount}、一般风险 ${generalCount}）`;
  }
  if (highCount > 0) {
    return `检测到 ${highCount} 项高风险敏感信息`;
  }
  return `检测到 ${generalCount} 项一般风险敏感信息`;
}

function formatPiiWarning(
  verdict: string,
  findings: unknown[],
  finalMessage = "本次仅提醒，未触发确认或阻断。",
  summary?: unknown,
): string {
  const typedFindings = findings.filter(
    (finding): finding is Record<string, unknown> =>
      typeof finding === "object" && finding !== null && !Array.isArray(finding),
  );
  return `[pii-checker] ${riskSummary(verdict, typedFindings, summary)}；${finalMessage}`;
}

function buildScanArgs(source: string, includeLowConfidence: boolean): string[] {
  const args = ["scan-pii", "--stdin", "--format", "json", "--redact-output", "--source", source];
  if (includeLowConfidence) {
    args.push("--include-low-confidence");
  }
  return args;
}

function getInboundText(event: any): string {
  return inboundPiiScanText(event);
}

function getModelOutputText(event: any): string {
  const response = safeString(event?.response);
  if (response.trim()) {
    return response;
  }
  const lastAssistant = safeString(event?.lastAssistant ?? event?.last_assistant);
  if (lastAssistant.trim()) {
    return lastAssistant;
  }
  const assistantTexts = Array.isArray(event?.assistantTexts)
    ? event.assistantTexts
    : Array.isArray(event?.assistant_texts)
      ? event.assistant_texts
      : [];
  return assistantTexts.filter((item: unknown) => typeof item === "string").join("\n");
}

function getToolOutputText(event: any): string {
  return afterToolCallPiiScanText(event);
}

async function scanPiiText(
  api: any,
  cfg: PiiScanConfig,
  event: any,
  ctx: any,
  text: string,
  source: string,
): Promise<{ verdict: string; findings: unknown[]; summary?: unknown } | undefined> {
  const result = await callAgentSecCli(buildScanArgs(source, cfg.includeLowConfidence), {
    timeout: CLI_TIMEOUT_MS,
    stdin: text,
    traceContext: buildTraceContext(event, ctx),
  });
  if (result.exitCode !== 0) {
    api.logger.warn(`[pii-checker] CLI failed: ${result.stderr || result.exitCode}`);
    return undefined;
  }

  let scanResult: { verdict?: unknown; findings?: unknown; summary?: unknown };
  try {
    scanResult = JSON.parse(result.stdout) as {
      verdict?: unknown;
      findings?: unknown;
      summary?: unknown;
    };
  } catch (error) {
    api.logger.warn(
      `[pii-checker] CLI returned invalid JSON, failed open: ${
        error instanceof Error ? error.message : String(error)
      }`,
    );
    return undefined;
  }
  return {
    verdict: safeString(scanResult.verdict) || "pass",
    findings: Array.isArray(scanResult.findings) ? scanResult.findings : [],
    summary: scanResult.summary,
  };
}

function logPiiWarning(
  api: any,
  verdict: string,
  findings: unknown[],
  cfg: PiiScanConfig,
  summary?: unknown,
  finalMessage?: string,
): string {
  const warning = formatPiiWarning(verdict, findings, finalMessage, summary);
  api.logger.debug?.(`[pii-checker] verdict=${verdict} policy=${cfg.policy}`);
  api.logger.warn(warning);
  return warning;
}

/**
 * 用户输入 PII / 凭据检测。
 *
 * Scans PII at the boundaries exposed by OpenClaw and applies the configured
 * policy. Unsupported confirmation or post-action boundaries fail open with a warning.
 */
export const piiScan: SecurityCapability = {
  id: "pii-scan-user-input",
  name: "PII Checker",
  hooks: ["before_dispatch", "before_tool_call", "after_tool_call", "llm_output"],
  register(api) {
    if (!envFlagEnabled("PII_CHECKER_HOOK_ENABLED", true)) {
      return;
    }
    const cfg = readConfig((api.pluginConfig as Record<string, any>) ?? {}, api);
    if (!cfg.scanUserInput) {
      api.logger.info("[pii-checker] piiScanUserInput=false, capability disabled");
      return;
    }

    api.on(
      "before_dispatch",
      async (event: any, ctx: any) => {
        try {
          const text = getInboundText(event);
          if (!text.trim()) {
            return undefined;
          }

          const scanResult = await scanPiiText(api, cfg, event, ctx, text, "user_input");
          if (scanResult === undefined) return undefined;
          const { verdict, findings, summary } = scanResult;

          if (verdict === "pass" || findings.length === 0) {
            api.logger.info("[pii-checker] pass");
            return undefined;
          }

          if (verdict !== "warn" && verdict !== "deny") {
            return undefined;
          }

          if (cfg.policy === "observe") return undefined;
          const warning = logPiiWarning(
            api,
            verdict,
            findings,
            cfg,
            summary,
            verdict === "deny" && cfg.policy === "block"
              ? "当前策略已阻断本次请求。"
              : verdict === "deny" && cfg.policy === "ask"
                ? "当前环节不支持确认/阻断，本次仅提醒，不会阻断。"
                : undefined,
          );
          if (verdict === "deny" && cfg.policy === "block") {
            return {
              handled: true,
              text: warning,
            };
          }
          return undefined;
        } catch (error) {
          api.logger.warn(
            `[pii-checker] failed open: ${error instanceof Error ? error.message : String(error)}`,
          );
          return undefined;
        }
      },
      { priority: BEFORE_DISPATCH_PRIORITY },
    );

    api.on(
      "before_tool_call",
      async (event: any, ctx: any) => {
        try {
          const text = valueToText(event?.params ?? event?.parameters ?? event?.args);
          if (!text.trim()) return undefined;
          const scanResult = await scanPiiText(api, cfg, event, ctx, text, "tool_input");
          if (scanResult === undefined) return undefined;
          const { verdict, findings, summary } = scanResult;
          if (verdict === "pass" || findings.length === 0) return undefined;
          if (verdict !== "warn" && verdict !== "deny") return undefined;
          if (cfg.policy === "observe") return undefined;
          const warning = logPiiWarning(
            api,
            verdict,
            findings,
            cfg,
            summary,
            verdict === "deny" && cfg.policy === "block"
              ? "当前策略已阻断本次工具调用。"
              : verdict === "deny" && cfg.policy === "ask"
                ? "当前策略要求确认，请确认后继续。"
                : undefined,
          );
          if (verdict === "deny" && cfg.policy === "ask") {
            return {
              requireApproval: {
                title: "PII Checker Security Review",
                description: warning,
                severity: "critical",
              },
            };
          }
          if (verdict === "deny" && cfg.policy === "block") {
            return { block: true, blockReason: warning };
          }
          return undefined;
        } catch (error) {
          api.logger.warn(
            `[pii-checker] failed open: ${error instanceof Error ? error.message : String(error)}`,
          );
          return undefined;
        }
      },
      { priority: BEFORE_DISPATCH_PRIORITY },
    );

    api.on("after_tool_call", async (event: any, ctx: any) => {
      try {
        const text = getToolOutputText(event);
        if (!text.trim()) return undefined;
        const scanResult = await scanPiiText(api, cfg, event, ctx, text, "tool_output");
        if (scanResult === undefined) return undefined;
        const { verdict, findings, summary } = scanResult;
        if (verdict === "pass" || findings.length === 0) return undefined;
        if (verdict !== "warn" && verdict !== "deny") return undefined;
        if (cfg.policy !== "observe") {
          const cannotEnforce =
            verdict === "deny" && (cfg.policy === "ask" || cfg.policy === "block");
          logPiiWarning(
            api,
            verdict,
            findings,
            cfg,
            summary,
            cannotEnforce
              ? "工具已经执行；当前环节不支持确认/阻断，本次仅提醒，工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。"
              : "工具已经执行；本次仅提醒，未触发确认或阻断，工具结果仍会进入模型上下文，已发生的外部副作用不会撤销。",
          );
        }
        return undefined;
      } catch (error) {
        api.logger.warn(
          `[pii-checker] failed open: ${error instanceof Error ? error.message : String(error)}`,
        );
        return undefined;
      }
    });

    api.on("llm_output", async (event: any, ctx: any) => {
      try {
        const text = getModelOutputText(event);
        if (!text.trim()) return undefined;
        const scanResult = await scanPiiText(api, cfg, event, ctx, text, "model_output");
        if (scanResult === undefined) return undefined;
        const { verdict, findings, summary } = scanResult;
        if (verdict === "pass" || findings.length === 0) return undefined;
        if (verdict !== "warn" && verdict !== "deny") return undefined;
        if (cfg.policy !== "observe") {
          logPiiWarning(
            api,
            verdict,
            findings,
            cfg,
            summary,
            "当前环节仅提醒，原始模型输出仍会交付，不会被脱敏或阻断。",
          );
        }
        return undefined;
      } catch (error) {
        api.logger.warn(
          `[pii-checker] failed open: ${error instanceof Error ? error.message : String(error)}`,
        );
        return undefined;
      }
    });
  },
};
