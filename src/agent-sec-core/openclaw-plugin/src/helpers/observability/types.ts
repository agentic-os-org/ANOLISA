import type { AgentSecObservabilityHookName } from "./schema.js";

export type UnknownRecord = Record<string, unknown>;

export type OpenClawObservabilityRecord = {
  hook: AgentSecObservabilityHookName;
  observedAt: string;
  metadata: UnknownRecord;
  metrics: UnknownRecord;
};
