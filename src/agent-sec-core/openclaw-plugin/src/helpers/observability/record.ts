import {
  OPENCLAW_TO_AGENT_SEC_HOOK,
  type AgentSecObservabilityHookName,
  type ObservabilityHookName,
} from "./schema.js";
import type {
  OpenClawObservabilityRecord,
  UnknownRecord,
} from "./types.js";
import {
  asRecord,
  compactRecord,
  firstString,
  isNonEmptyString,
} from "./helpers.js";
import { buildMetrics } from "./metrics.js";

export function buildOpenClawObservabilityRecord(
  hookName: ObservabilityHookName,
  event: unknown,
  ctx: unknown,
): OpenClawObservabilityRecord | undefined {
  const agentSecHookName = OPENCLAW_TO_AGENT_SEC_HOOK[hookName];
  const metadata = buildMetadata(event, ctx);
  if (!hasRequiredMetadata(agentSecHookName, metadata)) {
    return undefined;
  }

  const metrics = buildMetrics(hookName, event, ctx);
  if (Object.keys(metrics).length === 0) {
    return undefined;
  }

  return {
    hook: agentSecHookName,
    observedAt: new Date().toISOString(),
    metadata,
    metrics,
  };
}

function buildMetadata(event: unknown, ctx: unknown): UnknownRecord {
  const eventRecord = asRecord(event);
  const ctxRecord = asRecord(ctx);
  const eventTrace = asRecord(eventRecord?.trace);
  const ctxTrace = asRecord(ctxRecord?.trace);
  const runId = firstString(eventRecord?.runId, ctxRecord?.runId);

  return compactRecord({
    traceId: firstString(eventRecord?.traceId, ctxRecord?.traceId, eventTrace?.traceId, ctxTrace?.traceId),
    spanId: firstString(eventRecord?.spanId, ctxRecord?.spanId, eventTrace?.spanId, ctxTrace?.spanId),
    parentSpanId: firstString(
      eventRecord?.parentSpanId,
      ctxRecord?.parentSpanId,
      eventTrace?.parentSpanId,
      ctxTrace?.parentSpanId,
    ),
    runId,
    sessionId: firstString(eventRecord?.sessionId, ctxRecord?.sessionId),
    sessionKey: firstString(eventRecord?.sessionKey, ctxRecord?.sessionKey),
    toolCallId: firstString(eventRecord?.toolCallId, ctxRecord?.toolCallId),
    callId: firstString(eventRecord?.callId, ctxRecord?.callId),
  });
}

function hasRequiredMetadata(
  hookName: AgentSecObservabilityHookName,
  metadata: UnknownRecord,
): boolean {
  if (!isNonEmptyString(metadata.sessionId) || !isNonEmptyString(metadata.runId)) {
    return false;
  }

  if (hookName === "before_tool_call" || hookName === "after_tool_call") {
    return isNonEmptyString(metadata.toolCallId);
  }

  return true;
}
