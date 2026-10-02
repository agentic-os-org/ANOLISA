import type { ObservabilityHookName } from "./schema.js";
import type { UnknownRecord } from "./types.js";
import {
  afterToolCallPiiScanText,
  inboundPiiScanText,
  piiScanInputSha256,
} from "../pii-text.js";
import {
  asRecord,
  compactRecord,
  countHistoryMessages,
  firstString,
  getArray,
  getBoolean,
  getNumber,
  jsonByteLength,
  rawString,
} from "./helpers.js";
import { deriveToolResultError } from "./extractors.js";

export function buildMetrics(
  hookName: ObservabilityHookName,
  event: unknown,
  ctx: unknown,
): UnknownRecord {
  switch (hookName) {
    case "llm_input":
      return buildLlmInputMetrics(event, ctx);
    case "model_call_started":
      return buildModelCallStartedMetrics(event, ctx);
    case "model_call_ended":
      return buildModelCallEndedMetrics(event);
    case "llm_output":
      return buildLlmOutputMetrics(event, ctx);
    case "agent_end":
      return buildAgentEndMetrics(event);
    case "before_tool_call":
      return buildBeforeToolCallMetrics(event);
    case "after_tool_call":
      return buildAfterToolCallMetrics(event);
  }
}

function buildLlmInputMetrics(event: unknown, ctx: unknown): UnknownRecord {
  const record = asRecord(event);
  const ctxRecord = asRecord(ctx);
  const systemPrompt = rawString(record?.systemPrompt) ?? rawString(record?.system_prompt);
  const prompt =
    rawString(record?.prompt) ??
    rawString(record?.llmInput) ??
    rawString(record?.llm_input);
  const userInput =
    rawString(record?.userInput) ??
    rawString(record?.user_input) ??
    rawString(record?.userPrompt) ??
    rawString(record?.user_prompt);
  const images = getArray(record?.images);

  return compactRecord({
    prompt,
    system_prompt: systemPrompt,
    user_input: userInput,
    pii_scan_input_sha256: piiScanInputSha256(inboundPiiScanText(event)),
    history_messages_count:
      getNumber(record, "historyMessagesCount") ??
      getNumber(record, "history_messages_count") ??
      countHistoryMessages(record?.historyMessages ?? record?.history_messages ?? record?.messages),
    model_id: modelId(record, ctxRecord),
    model_provider: modelProvider(record, ctxRecord),
    images_count:
      getNumber(record, "imagesCount") ??
      getNumber(record, "images_count") ??
      (images === undefined ? undefined : images.length),
    context_window_utilization:
      getNumber(record, "contextWindowUtilization") ??
      getNumber(record, "context_window_utilization"),
  });
}

function buildModelCallStartedMetrics(event: unknown, ctx: unknown): UnknownRecord {
  const record = asRecord(event);
  const ctxRecord = asRecord(ctx);
  return compactRecord({
    model_id: modelId(record, ctxRecord),
    model_provider: modelProvider(record, ctxRecord),
    api: firstString(record?.api, record?.modelApi, record?.model_api),
    transport: firstString(record?.transport, record?.networkTransport, record?.network_transport),
  });
}

function buildModelCallEndedMetrics(event: unknown): UnknownRecord {
  const record = asRecord(event);
  return compactRecord({
    latency_ms:
      getNumber(record, "latencyMs") ??
      getNumber(record, "latency_ms") ??
      getNumber(record, "durationMs") ??
      getNumber(record, "duration_ms"),
    outcome: rawString(record?.outcome),
    error_category: rawString(record?.errorCategory) ?? rawString(record?.error_category),
    failure_kind: rawString(record?.failureKind) ?? rawString(record?.failure_kind),
    request_payload_bytes:
      getNumber(record, "requestPayloadBytes") ?? getNumber(record, "request_payload_bytes"),
    response_stream_bytes:
      getNumber(record, "responseStreamBytes") ?? getNumber(record, "response_stream_bytes"),
    time_to_first_byte_ms:
      getNumber(record, "timeToFirstByteMs") ?? getNumber(record, "time_to_first_byte_ms"),
    upstream_request_id_hash:
      rawString(record?.upstreamRequestIdHash) ?? rawString(record?.upstream_request_id_hash),
  });
}

function buildLlmOutputMetrics(event: unknown, ctx: unknown): UnknownRecord {
  const record = asRecord(event);
  const assistantTextItems = getArray(record?.assistantTexts ?? record?.assistant_texts);
  const assistantTexts = getStringArray(record?.assistantTexts ?? record?.assistant_texts);
  const lastAssistant = record?.lastAssistant ?? record?.last_assistant;
  const lastAssistantRecord = asRecord(lastAssistant);
  const response =
    rawString(record?.response) ??
    rawString(lastAssistant) ??
    rawString(record?.last_assistant) ??
    lastString(assistantTexts);
  const stopReason =
    firstString(lastAssistantRecord?.stopReason, lastAssistantRecord?.stop_reason) ??
    (response === undefined ? undefined : "stop");
  const toolCalls = extractToolCallSummaries(record, lastAssistantRecord);
  const toolCallsCount = toolCalls.length;

  return compactRecord({
    response,
    output_kind: deriveLlmOutputKind(response, stopReason, toolCallsCount, lastAssistantRecord),
    stop_reason: stopReason,
    assistant_texts_count: assistantTextItems === undefined ? undefined : (assistantTexts ?? []).length,
    tool_calls_count: toolCallsCount === 0 ? undefined : toolCallsCount,
    tool_calls: toolCallsCount === 0 ? undefined : toolCalls,
  });
}

function buildAgentEndMetrics(event: unknown): UnknownRecord {
  const record = asRecord(event);
  return compactRecord({
    success: getBoolean(record, "success"),
    error: rawString(record?.error),
    duration_ms:
      getNumber(record, "durationMs") ??
      getNumber(record, "duration_ms") ??
      getNumber(record, "duration"),
    total_api_calls: getNumber(record, "totalApiCalls") ?? getNumber(record, "total_api_calls"),
    total_tool_calls:
      getNumber(record, "totalToolCalls") ?? getNumber(record, "total_tool_calls"),
    final_model_id:
      firstString(record?.finalModelId, record?.final_model_id, record?.model, record?.modelId),
    final_model_provider:
      firstString(
        record?.finalModelProvider,
        record?.final_model_provider,
        record?.provider,
        record?.modelProvider,
      ),
  });
}

function buildBeforeToolCallMetrics(event: unknown): UnknownRecord {
  const record = asRecord(event);
  return compactRecord({
    tool_name: rawString(record?.toolName) ?? rawString(record?.tool_name),
    parameters: record?.params ?? record?.parameters ?? record?.args,
  });
}

function buildAfterToolCallMetrics(event: unknown): UnknownRecord {
  const record = asRecord(event);
  const resultRecord = asRecord(record?.result);
  const details = asRecord(resultRecord?.details);
  const error =
    rawString(record?.error) ??
    deriveToolResultError(record?.result, getBoolean(record, "isError") ?? getBoolean(record, "is_error"));

  return compactRecord({
    result: record?.result,
    error,
    pii_scan_input_sha256: piiScanInputSha256(afterToolCallPiiScanText(event)),
    duration_ms:
      getNumber(record, "durationMs") ??
      getNumber(record, "duration_ms") ??
      getNumber(record, "duration") ??
      getNumber(details, "durationMs") ??
      getNumber(details, "duration_ms"),
    status:
      rawString(record?.status) ??
      rawString(record?.toolStatus) ??
      rawString(record?.tool_status) ??
      rawString(details?.status),
    exit_code:
      getNumber(record, "exitCode") ??
      getNumber(record, "exit_code") ??
      getNumber(details, "exitCode") ??
      getNumber(details, "exit_code"),
    result_size_bytes:
      getNumber(record, "resultSizeBytes") ??
      getNumber(record, "result_size_bytes") ??
      jsonByteLength(record?.result),
  });
}

function modelId(record: UnknownRecord | undefined, ctxRecord: UnknownRecord | undefined): string | undefined {
  return firstString(record?.model, record?.modelId, record?.model_id, ctxRecord?.modelId, ctxRecord?.model_id);
}

function modelProvider(record: UnknownRecord | undefined, ctxRecord: UnknownRecord | undefined): string | undefined {
  return firstString(
    record?.provider,
    record?.modelProvider,
    record?.model_provider,
    ctxRecord?.modelProviderId,
    ctxRecord?.model_provider,
  );
}

function getStringArray(value: unknown): string[] | undefined {
  const items = getArray(value);
  if (items === undefined) {
    return undefined;
  }
  const strings = items.filter((item): item is string => typeof item === "string");
  return strings;
}

function lastString(items: string[] | undefined): string | undefined {
  return items === undefined ? undefined : items.at(-1);
}

function deriveLlmOutputKind(
  response: string | undefined,
  stopReason: string | undefined,
  toolCallsCount: number,
  lastAssistant: UnknownRecord | undefined,
): string {
  if (toolCallsCount > 0 || stopReason === "toolUse") {
    return "tool_use";
  }
  if (stopReason === "error") {
    return "error";
  }
  if (response !== undefined) {
    return "text";
  }
  if (lastAssistant !== undefined) {
    return "structured";
  }
  return "empty";
}

function extractToolCallSummaries(
  record: UnknownRecord | undefined,
  lastAssistant: UnknownRecord | undefined,
): UnknownRecord[] {
  const candidates = [
    ...(getArray(record?.tool_calls) ?? []),
    ...(getArray(record?.toolCalls) ?? []),
    ...(getArray(lastAssistant?.tool_calls) ?? []),
    ...(getArray(lastAssistant?.toolCalls) ?? []),
    ...(getArray(lastAssistant?.content) ?? []),
  ];

  const summaries: UnknownRecord[] = [];
  for (const candidate of candidates) {
    const summary = toolCallSummary(candidate);
    if (summary !== undefined) {
      summaries.push(summary);
    }
  }
  return summaries;
}

function toolCallSummary(value: unknown): UnknownRecord | undefined {
  const record = asRecord(value);
  if (record === undefined || !isToolCallRecord(record)) {
    return undefined;
  }

  const functionRecord = asRecord(record.function);
  const toolCallId = firstString(
    record.toolCallId,
    record.tool_call_id,
    record.toolUseId,
    record.tool_use_id,
    record.id,
  );
  const toolName = firstString(
    record.toolName,
    record.tool_name,
    record.name,
    functionRecord?.name,
  );
  const parameters =
    parseToolArguments(functionRecord?.arguments) ??
    record.parameters ??
    record.params ??
    record.args ??
    record.input ??
    functionRecord?.parameters ??
    functionRecord?.params ??
    functionRecord?.args;

  const summary = compactRecord({
    toolCallId,
    toolName,
    parameters,
  });
  return Object.keys(summary).length === 0 ? undefined : summary;
}

function isToolCallRecord(record: UnknownRecord): boolean {
  const type = firstString(record.type, record.kind);
  if (type === undefined) {
    return Boolean(
      record.function !== undefined ||
      record.toolName !== undefined ||
      record.tool_name !== undefined ||
      record.name !== undefined,
    );
  }
  return [
    "toolCall",
    "toolUse",
    "tool_call",
    "tool_use",
    "functionCall",
    "function_call",
    "function",
  ].includes(type);
}

function parseToolArguments(value: unknown): unknown {
  if (typeof value !== "string") {
    return undefined;
  }
  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}
