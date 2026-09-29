import type { OpenClawPluginApi } from "openclaw/plugin-sdk/plugin-entry";
import type { SecurityCapability } from "../types.js";
import { envFlagEnabled, recordOpenClawObservability } from "../utils.js";
import type { CliResult } from "../utils.js";
import {
  OBSERVABILITY_HOOKS,
  type ObservabilityHookName,
} from "../helpers/observability/schema.js";
import { formatSafeError } from "../helpers/observability/helpers.js";
import { buildOpenClawObservabilityRecord } from "../helpers/observability/record.js";

export { buildOpenClawObservabilityRecord } from "../helpers/observability/record.js";

const OBSERVABILITY_PRIORITY = 1000;
const OBSERVABILITY_LATE_PRIORITY = -10_000;
const LOG_DETAIL_MAX_CHARS = 1000;

export const observability: SecurityCapability = {
  id: "observability",
  name: "OpenClaw Observability",
  hooks: [...OBSERVABILITY_HOOKS],
  register(api) {
    const hookEnabled = envFlagEnabled("OBSERVABILITY_HOOK_ENABLED", true);
    api.on(
      "llm_input",
      (event, ctx) => observeHook(api, hookEnabled, "llm_input", event, ctx),
      { priority: OBSERVABILITY_PRIORITY },
    );
    api.on(
      "model_call_started",
      (event, ctx) => observeHook(api, hookEnabled, "model_call_started", event, ctx),
      { priority: OBSERVABILITY_PRIORITY },
    );
    api.on(
      "model_call_ended",
      (event, ctx) => observeHook(api, hookEnabled, "model_call_ended", event, ctx),
      { priority: OBSERVABILITY_PRIORITY },
    );
    api.on(
      "llm_output",
      (event, ctx) => observeHook(api, hookEnabled, "llm_output", event, ctx),
      { priority: OBSERVABILITY_PRIORITY },
    );
    api.on(
      "agent_end",
      (event, ctx) => observeHook(api, hookEnabled, "agent_end", event, ctx),
      { priority: OBSERVABILITY_PRIORITY },
    );
    api.on(
      "before_tool_call",
      (event, ctx) => observeHook(api, hookEnabled, "before_tool_call", event, ctx),
      { priority: OBSERVABILITY_LATE_PRIORITY },
    );
    api.on(
      "after_tool_call",
      (event, ctx) => observeHook(api, hookEnabled, "after_tool_call", event, ctx),
      { priority: OBSERVABILITY_PRIORITY },
    );
  },
};

function observeHook(
  api: OpenClawPluginApi,
  hookEnabled: boolean,
  hookName: ObservabilityHookName,
  event: unknown,
  ctx: unknown,
): void {
  if (!hookEnabled) {
    return;
  }

  try {
    const payload = buildOpenClawObservabilityRecord(hookName, event, ctx);
    if (payload === undefined) {
      return;
    }
    void recordOpenClawObservability(payload)
      .then((result) => {
        if (result.exitCode !== 0) {
          api.logger.warn?.(formatRecordFailure(hookName, payload.hook, result));
        }
      })
      .catch((error: unknown) => {
        api.logger.warn?.(
          `[observability] record error source_hook=${hookName} record_hook=${formatLogValue(payload.hook)} error=${formatLogError(error)}`,
        );
      });
  } catch (error) {
    api.logger.warn?.(`[observability] failed to build ${hookName} payload: ${formatSafeError(error)}`);
  }
}

function formatRecordFailure(
  sourceHook: ObservabilityHookName,
  recordHook: unknown,
  result: CliResult,
): string {
  const fields = [
    "[observability] record failed",
    `source_hook=${sourceHook}`,
    `record_hook=${formatLogValue(recordHook)}`,
    `exit=${result.exitCode}`,
  ];
  const stderr = formatLogValue(result.stderr);
  if (stderr) {
    fields.push(`stderr=${stderr}`);
  }
  const stdout = formatLogValue(result.stdout);
  if (stdout) {
    fields.push(`stdout=${stdout}`);
  }
  return fields.join(" ");
}

function formatLogError(error: unknown): string {
  if (error instanceof Error) {
    const message = formatLogValue(error.message);
    return message ? `${error.name}: ${message}` : error.name;
  }
  return `${typeof error}: ${formatLogValue(error)}`;
}

function formatLogValue(value: unknown): string {
  if (value === undefined || value === null) {
    return "";
  }
  const text = String(value).trim().replace(/\s+/g, " ");
  if (text.length <= LOG_DETAIL_MAX_CHARS) {
    return text;
  }
  return `${text.slice(0, LOG_DETAIL_MAX_CHARS)}...<truncated>`;
}
