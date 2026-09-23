import type { OpenClawPluginApi } from "openclaw/plugin-sdk/plugin-entry";

export const OBSERVABILITY_HOOKS = [
  "llm_input",
  "model_call_started",
  "model_call_ended",
  "llm_output",
  "agent_end",
  "before_tool_call",
  "after_tool_call",
] as const satisfies readonly Parameters<OpenClawPluginApi["on"]>[0][];

export type ObservabilityHookName = (typeof OBSERVABILITY_HOOKS)[number];

export type AgentSecObservabilityHookName =
  | "before_agent_run"
  | "before_llm_call"
  | "after_llm_call"
  | "before_tool_call"
  | "after_tool_call"
  | "after_agent_run";

export const OPENCLAW_TO_AGENT_SEC_HOOK: Record<ObservabilityHookName, AgentSecObservabilityHookName> = {
  llm_input: "before_agent_run",
  model_call_started: "before_llm_call",
  model_call_ended: "after_llm_call",
  llm_output: "after_agent_run",
  before_tool_call: "before_tool_call",
  after_tool_call: "after_tool_call",
  agent_end: "after_agent_run",
};

// TODO: generate agent sec metric allowlist from ground truth
export const AGENT_SEC_METRIC_ALLOWLIST: Record<AgentSecObservabilityHookName, readonly string[]> = {
  before_agent_run: [
    "prompt",
    "system_prompt",
    "user_input",
    "pii_scan_input_sha256",
    "history_messages_count",
    "images_count",
    "context_window_utilization",
    "model_id",
    "model_provider",
  ],
  before_llm_call: [
    "prompt",
    "system_prompt",
    "user_input",
    "history_messages_count",
    "images_count",
    "context_window_utilization",
    "model_id",
    "model_provider",
    "api",
    "transport",
  ],
  after_llm_call: [
    "latency_ms",
    "outcome",
    "error_category",
    "failure_kind",
    "request_payload_bytes",
    "response",
    "output_kind",
    "stop_reason",
    "assistant_texts_count",
    "tool_calls_count",
    "tool_calls",
    "response_stream_bytes",
    "time_to_first_byte_ms",
    "upstream_request_id_hash",
  ],
  before_tool_call: [
    "tool_name",
    "parameters",
  ],
  after_tool_call: [
    "result",
    "error",
    "pii_scan_input_sha256",
    "duration_ms",
    "status",
    "exit_code",
    "result_size_bytes",
  ],
  after_agent_run: [
    "response",
    "output_kind",
    "stop_reason",
    "assistant_texts_count",
    "tool_calls_count",
    "tool_calls",
    "success",
    "error",
    "duration_ms",
    "total_api_calls",
    "total_tool_calls",
    "final_model_id",
    "final_model_provider",
  ],
};
