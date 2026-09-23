/**
 * AgentSight backend API client
 *
 * When the frontend is served by agentsight itself (embedded mode), the API
 * is on the same host/port as the page — use window.location.origin.
 * Otherwise fall back to REACT_APP_API_BASE or localhost:7396 for local dev.
 */

import type {
  OptimizeSessionResults,
  OptimizeLlmConfig,
} from '../types/optimization';

const API_BASE: string = (() => {
  // Explicit override via env var (set at build time for non-embedded deployments)
  if (typeof process !== 'undefined' && (process.env as any).REACT_APP_API_BASE) {
    return (process.env as any).REACT_APP_API_BASE as string;
  }
  // In a real browser context, use the same origin so it works on any host/port
  if (typeof window !== 'undefined' && window.location && window.location.origin) {
    return window.location.origin;
  }
  // Fallback for local dev (webpack-dev-server on :3000 → backend on :7396)
  return 'http://localhost:7396';
})();

// ─── Types mirroring backend response structs ────────────────────────────────

export interface SessionSummary {
  session_id: string;
  conversation_count: number;
  first_seen_ns: number;
  last_seen_ns: number;
  total_input_tokens: number;
  total_output_tokens: number;
  model: string | null;
  agent_name: string | null;
  /** Earliest user query in the window (≤ 200 chars, best-effort) */
  first_user_query?: string | null;
  /** Latest user query in the window (≤ 200 chars, best-effort) */
  last_user_query?: string | null;
}

export interface TraceSummary {
  conversation_id: string;
  call_count: number;
  total_input_tokens: number;
  total_output_tokens: number;
  start_ns: number;
  end_ns: number | null;
  model: string | null;
  /** First user_query recorded in this conversation (best-effort) */
  user_query: string | null;
}

export interface TraceEventDetail {
  id: number;
  call_id: string | null;
  start_timestamp_ns: number;
  end_timestamp_ns: number | null;
  model: string | null;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
  /** Raw JSON string — parse before use */
  input_messages: string | null;
  /** Raw JSON string — parse before use */
  output_messages: string | null;
  /** Raw JSON string — parse before use */
  system_instructions: string | null;
  agent_name: string | null;
  process_name: string | null;
  pid: number | null;
  /** The user query that triggered this LLM call */
  user_query: string | null;
  /** Raw full event JSON — fallback when output_messages is null */
  event_json: string | null;
  /** Conversation ID (user query fingerprint) */
  conversation_id: string | null;
}

export interface ResourceSample {
  timestamp_ns: number;
  pid: number;
  agent_name: string | null;
  cpu_percent: number;
  memory_bytes: number;
}

export interface SessionPhase {
  kind: 'llm' | 'tool_call' | 'idle';
  start_timestamp_ns: number;
  end_timestamp_ns: number;
  tool_call_id?: string;
}

export interface SessionResourceTimeline {
  session_id: string;
  samples: ResourceSample[];
  phases: SessionPhase[];
}

// ─── Internal helpers ────────────────────────────────────────────────────────

export class ApiRequestError extends Error {
  readonly status: number;
  readonly body: Record<string, unknown> | null;

  constructor(url: string, status: number, text: string, body: Record<string, unknown> | null) {
    super(`API ${url} -> ${status}: ${text}`);
    this.name = 'ApiRequestError';
    this.status = status;
    this.body = body;
  }
}

async function apiFetch<T>(url: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(url, { ...init, credentials: 'same-origin' });
  if (res.status === 401) {
    // Session expired or invalid — redirect to login
    window.location.hash = '#/login';
    throw new Error('Authentication required');
  }
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    let body: Record<string, unknown> | null = null;
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        body = parsed as Record<string, unknown>;
      }
    } catch {
      // Error responses may be plain text.
    }
    throw new ApiRequestError(url, res.status, text, body);
  }
  return res.json() as Promise<T>;
}

// ─── Trajectory reuse labels ────────────────────────────────────────────────

/**
 * Runs the deterministic labelling pass.
 *
 * Safe to call repeatedly: unchanged content under unchanged rules is skipped,
 * so this is a refresh rather than a rebuild.
 */
export async function runReuseTriage(limit?: number): Promise<TriageReport> {
  const qs = limit === undefined ? '' : `?limit=${limit}`;
  return apiFetch<TriageReport>(`${API_BASE}/api/reuse/triage${qs}`, { method: 'POST' });
}

/** Lists labelled trajectories, newest decision first. */
export async function fetchReuseSessions(filter: {
  label?: TrajectoryLabel;
  confirmState?: ConfirmState;
  limit?: number;
} = {}): Promise<SessionsResponse> {
  const params = new URLSearchParams();
  if (filter.label) params.set('label', filter.label);
  if (filter.confirmState) params.set('confirm_state', filter.confirmState);
  if (filter.limit !== undefined) params.set('limit', String(filter.limit));
  const qs = params.toString() ? `?${params.toString()}` : '';
  return apiFetch<SessionsResponse>(`${API_BASE}/api/reuse/sessions${qs}`);
}

/**
 * Records a human decision on one label.
 *
 * `confirm` endorses the automatic verdict; `override` replaces it. Both count
 * as a person having spoken.
 */
export async function decideReuseLabel(
  sessionId: string,
  decision: {
    action: 'confirm' | 'override';
    label?: TrajectoryLabel;
    reason?: string;
    decidedBy?: string;
  },
): Promise<SessionLabelView> {
  return apiFetch<SessionLabelView>(
    `${API_BASE}/api/reuse/sessions/${encodeURIComponent(sessionId)}/label`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        action: decision.action,
        label: decision.label,
        reason: decision.reason,
        decided_by: decision.decidedBy,
      }),
    },
  );
}

/** Endorses several automatic verdicts at once; ids that are not there are skipped. */
export async function confirmReuseLabels(
  sessionIds: string[],
  decidedBy?: string,
): Promise<{ confirmed: number; session_ids: string[] }> {
  return apiFetch<{ confirmed: number; session_ids: string[] }>(
    `${API_BASE}/api/reuse/sessions/labels:batch-confirm`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ session_ids: sessionIds, decided_by: decidedBy }),
    },
  );
}

/**
 * Per-rule counts of how often a person accepted or overturned the verdict it
 * contributed to — the only continuously available measure of which rules
 * misfire.
 */
export async function fetchReuseLabelStats(): Promise<LabelStatsResponse> {
  return apiFetch<LabelStatsResponse>(`${API_BASE}/api/reuse/label-stats`);
}

/**
 * Asks the model to label trajectories the rules could not place.
 *
 * Every judgement is a paid request, so this is off unless the server was
 * configured for it, and answers 503 when it is not.
 */
export async function runReuseJudgements(
  input: { sessionIds?: string[]; limit?: number } = {},
): Promise<JudgeReport> {
  return apiFetch<JudgeReport>(`${API_BASE}/api/reuse/judge`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ session_ids: input.sessionIds, limit: input.limit }),
  });
}

// ─── Enforcement APIs ───────────────────────────────────────────────────────

export interface EnforcementHealth {
  ready: boolean;
  backend: string;
  capabilities: EnforcementCapabilities;
  message: string | null;
}

export interface EnforcementCapabilities {
  max_active_bindings?: number | null;
  credential_observe: boolean;
  credential_audit: boolean;
  credential_enforce: boolean;
  policy_handoff: boolean;
  alternate_pid_retarget: boolean;
  test_development: boolean;
}

export interface EnforcementBinding {
  request: {
    binding_id: string;
    agent_id: string;
    session_id: string | null;
    root_pid: number;
    process_start_time: number;
    policy_id: string;
    policy_revision: string;
    policy_dsl: string;
    policy_mode?: EnforcementPolicyMode | null;
  };
  state: 'pending' | 'enforced' | 'failed' | 'degraded' | 'detaching' | 'detached';
  message: string | null;
  domain_id: number | null;
}

export interface EnforcementViolation {
  event_id: string;
  binding_id: string;
  agent_id: string;
  session_id: string | null;
  policy_id: string;
  policy_revision: string;
  pid: number;
  ppid: number | null;
  process_start_time: number;
  operation: string;
  target: string;
  effect: 'notify' | 'block' | 'kill';
  blocked: boolean;
  killed: boolean;
  rule_id: string | null;
  reason: string | null;
  occurred_at_ns: number;
  observed_at_ns: number;
  actplane_revision: string;
  /** Correlated risk case id (enriched by the backend), when available. */
  case_id?: string | null;
}

export interface FileBindingInput {
  agent_id: string;
  session_id?: string;
  root_pid: number;
  path: string;
}

export type EnforcementPolicyMode = 'observe' | 'audit' | 'enforce';
export type CredentialDestinationScope = 'public_ipv4';

export function enforcementSupportsMode(
  health: EnforcementHealth | null,
  mode: EnforcementPolicyMode,
): boolean {
  if (health?.ready !== true) return false;
  if (mode === 'observe') return health.capabilities.credential_observe;
  if (mode === 'audit') return health.capabilities.credential_audit;
  return health.capabilities.credential_enforce;
}

export function enforcementSupportsContainment(health: EnforcementHealth | null): boolean {
  return health?.ready === true
    && health.capabilities.credential_enforce
    && health.capabilities.policy_handoff;
}

export function enforcementViolationTotal(
  violations: ReadonlyArray<Pick<EnforcementViolation, 'blocked'>>,
  health: EnforcementHealth | null,
): number {
  if (health?.capabilities.credential_enforce === true) {
    return violations.filter((event) => event.blocked).length;
  }
  return violations.length;
}

export interface CredentialBindingInput {
  agent_id: string;
  session_id?: string;
  root_pid: number;
  source_path?: string;
  source_paths?: string[];
  trusted_endpoint?: string;
  trusted_endpoints?: string[];
  revision: number;
  mode: EnforcementPolicyMode;
  taint_ttl_secs?: number;
  destination_scope: CredentialDestinationScope;
}

export interface AgentProtectionPreview {
  agent_id: string;
  root_pid: number;
  workspace_path: string;
  source_paths: string[];
  trusted_endpoints: string[];
  mode: EnforcementPolicyMode;
  max_trusted_endpoints: number;
}

export class EnforcementApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    message: string,
    public readonly retryable: boolean,
  ) {
    super(message);
    this.name = 'EnforcementApiError';
  }
}

async function enforcementRequest<T>(path: string, init: RequestInit = {}): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    ...init,
    credentials: 'same-origin',
  });
  if (res.status === 401) {
    window.location.hash = '#/login';
    throw new Error('Authentication required');
  }
  if (!res.ok) {
    const body = await res.json().catch(() => null) as {
      error?: { code?: string; message?: string; retryable?: boolean };
    } | null;
    const error = body?.error;
    if (error && typeof error.code === 'string' && typeof error.message === 'string') {
      throw new EnforcementApiError(
        res.status,
        error.code,
        error.message,
        Boolean(error.retryable),
      );
    }
    throw new EnforcementApiError(
      res.status,
      'enforcement_request_failed',
      res.statusText || 'Enforcement request failed',
      false,
    );
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return res.json() as Promise<T>;
}

export const fetchEnforcementHealth = () =>
  enforcementRequest<EnforcementHealth>('/api/enforcement/health');

export const fetchEnforcementBindings = () =>
  enforcementRequest<{ bindings: EnforcementBinding[] }>('/api/enforcement/bindings');

export const fetchEnforcementViolations = (limit = 100) =>
  enforcementRequest<{ violations: EnforcementViolation[] }>(
    `/api/enforcement/violations?limit=${Math.min(1000, Math.max(1, limit))}`,
  );

export const createFileBinding = (input: FileBindingInput) =>
  enforcementRequest<EnforcementBinding>('/api/enforcement/file-bindings', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(input),
  });

export const createCredentialBinding = (input: CredentialBindingInput) =>
  enforcementRequest<EnforcementBinding>('/api/enforcement/credential-bindings', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(input),
  });

export const fetchAgentProtectionPreview = (pid: number, directory?: string) => {
  const query = directory ? `?directory=${encodeURIComponent(directory)}` : '';
  return enforcementRequest<AgentProtectionPreview>(
    `/api/enforcement/agent-protection/${pid}${query}`,
  );
};

export const detachEnforcementBinding = (bindingId: string) =>
  enforcementRequest<void>(
    `/api/enforcement/bindings/${encodeURIComponent(bindingId)}`,
    { method: 'DELETE' },
  );

// ─── Public API ──────────────────────────────────────────────────────────────

/**
 * List sessions within a nanosecond time range.
 * Defaults to the last 24 h when omitted.
 */
export async function fetchSessions(
  startNs?: number,
  endNs?: number
): Promise<SessionSummary[]> {
  const params = new URLSearchParams();
  if (startNs !== undefined) params.set('start_ns', String(startNs));
  if (endNs !== undefined) params.set('end_ns', String(endNs));
  const qs = params.toString() ? `?${params.toString()}` : '';
  return apiFetch<SessionSummary[]>(`${API_BASE}/api/sessions${qs}`);
}

/** Candidate session summary sent to the semantic search endpoint. */
export interface SemanticSearchCandidate {
  session_id: string;
  first_message: string | null;
  last_message: string | null;
  project: string | null;
}

/** One LLM-ranked semantic match. */
export interface SemanticSearchResult {
  session_id: string;
  relevance: 'high' | 'medium';
  reason: string;
}

export interface SemanticSearchResponse {
  results: SemanticSearchResult[];
}

/**
 * Ask the configured LLM to rank candidate sessions by semantic relevance to
 * `query`. Returns an empty list when semantic search is unavailable or the
 * caller sent too few candidates.
 */
export async function semanticSearchSessions(body: {
  query: string;
  candidates: SemanticSearchCandidate[];
}): Promise<SemanticSearchResponse> {
  return apiFetch<SemanticSearchResponse>(`${API_BASE}/api/sessions/search`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

/**
 * List all trace IDs within a session, with per-trace token stats.
 * Optional startNs/endNs are forwarded as query parameters for future
 * backend-side time-range filtering.
 */
export async function fetchTraces(
  sessionId: string,
  startNs?: number | null,
  endNs?: number | null,
): Promise<TraceSummary[]> {
  const params = new URLSearchParams();
  if (startNs != null) params.set('start_ns', String(startNs));
  if (endNs != null) params.set('end_ns', String(endNs));
  const qs = params.toString();
  const suffix = qs ? `?${qs}` : '';
  return apiFetch<TraceSummary[]>(
    `${API_BASE}/api/sessions/${encodeURIComponent(sessionId)}/traces${suffix}`
  );
}

/** Fetch process CPU/RSS observations and inferred activity phases for a Session. */
export async function fetchSessionResources(
  sessionId: string,
  startNs?: number | null,
  endNs?: number | null,
  maxPoints = 2_000,
): Promise<SessionResourceTimeline> {
  const params = new URLSearchParams({ max_points: String(maxPoints) });
  if (startNs != null) params.set('start_ns', String(startNs));
  if (endNs != null) params.set('end_ns', String(endNs));
  return apiFetch<SessionResourceTimeline>(
    `${API_BASE}/api/sessions/${encodeURIComponent(sessionId)}/resources?${params.toString()}`,
  );
}

// ─── Collected trajectory APIs ───────────────────────────────────────────────

/** Summary row from trajectories.db (log-collected sessions). */
export interface TrajectorySummary {
  session_id: string;
  schema_version: string;
  agent_name: string;
  model_name: string | null;
  num_steps: number;
  total_prompt_tokens: number | null;
  total_completion_tokens: number | null;
  start_time: string | null;
  end_time: string | null;
  /** First user-authored message preview (≤ 200 chars) from the ATIF steps */
  first_user_message?: string | null;
  /** Last user-authored message preview (≤ 200 chars) from the ATIF steps */
  last_user_message?: string | null;
  project: string;
  source: string;
  is_subagent: boolean;
  collected_at_ns: number;
}

/**
 * List log-collected trajectories (newest first). Returns an empty list when
 * trajectory collection has never run (graceful degradation).
 */
export async function fetchTrajectories(limit = 1000): Promise<TrajectorySummary[]> {
  return apiFetch<TrajectorySummary[]>(`${API_BASE}/api/trajectories?limit=${limit}`);
}

/**
 * Fetch detailed LLM call events for a conversation (user query).
 */
export async function fetchConversationDetail(conversationId: string): Promise<TraceEventDetail[]> {
  return apiFetch<TraceEventDetail[]>(
    `${API_BASE}/api/conversations/${encodeURIComponent(conversationId)}`
  );
}

// ─── Grader APIs ─────────────────────────────────────────────────────────────

export type EvaluationVerdict = 'pass' | 'warn' | 'fail';
export type EvaluationRootCause =
  | 'none'
  | 'no_final_answer'
  | 'interrupted_main_call'
  | 'agent_crash'
  | 'runtime_error'
  | 'tool_failure'
  | 'safety_risk'
  | 'loop_detected'
  | 'excessive_cost'
  | 'partial_snapshot';

export interface EvaluationTarget {
  conversation_id: string;
  trace_id?: string | null;
  call_id?: string | null;
  step_id?: string | null;
}

export interface EvaluationRef {
  type: 'genai_event' | 'interruption' | 'security_event' | 'trace' | 'tool_call' | 'atif_step';
  id: string;
  label: string;
  severity?: string | null;
  target: EvaluationTarget;
  deeplink?: {
    route: string;
    query: Record<string, unknown>;
  } | null;
  metadata?: Record<string, unknown> | null;
}

export interface EvaluationDimension {
  name: string;
  score: number;
  verdict: EvaluationVerdict;
  reason: string;
  evidence_refs: EvaluationRef[];
}

export interface EvaluationFinding {
  code: string;
  severity: string;
  message: string;
  evidence_refs: EvaluationRef[];
}

export interface EvaluationMetadata {
  evaluated_with_pending: boolean;
  pending_call_count: number;
  input_event_count: number;
  grader_type: 'rule' | 'llm' | 'agent';
  grader_version: string;
  rubric_version: string | null;
  judge_model: string | null;
  prompt_hash: string | null;
  confidence: number | null;
}

export interface EvaluationResult {
  target_type: 'conversation';
  target_id: string;
  run_id: string;
  input_hash: string;
  verdict: EvaluationVerdict;
  score: number;
  summary: string;
  root_cause: EvaluationRootCause;
  recommended_action: string;
  dimensions: EvaluationDimension[];
  findings: EvaluationFinding[];
  metadata: EvaluationMetadata;
}

export interface EvaluationResponse {
  result: EvaluationResult;
  reused_existing_run: boolean;
}

export class EvaluationNotReadyError extends Error {
  readonly pendingCallCount: number;

  constructor(message: string, pendingCallCount: number) {
    super(message);
    this.name = 'EvaluationNotReadyError';
    this.pendingCallCount = pendingCallCount;
  }
}

/**
 * Fetch the latest persisted evaluation for a conversation.
 */
export async function fetchLatestEvaluation(conversationId: string): Promise<EvaluationResult | null> {
  const params = new URLSearchParams({
    target_type: 'conversation',
    target_id: conversationId,
  });
  return apiFetch<EvaluationResult | null>(`${API_BASE}/api/grader/latest?${params.toString()}`);
}

/**
 * Manually evaluate a conversation with the rule-based grader.
 */
export async function evaluateConversation(
  conversationId: string,
  force = false,
): Promise<EvaluationResponse> {
  try {
    return await apiFetch<EvaluationResponse>(`${API_BASE}/api/grader/evaluate`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        target_type: 'conversation',
        target_id: conversationId,
        force,
      }),
    });
  } catch (error) {
    if (
      error instanceof ApiRequestError &&
      error.status === 409 &&
      error.body?.error === 'conversation_not_ready'
    ) {
      const message = error.body.message;
      throw new EvaluationNotReadyError(
        typeof message === 'string' ? message : 'Conversation still has pending LLM calls.',
        Number(error.body.pending_call_count ?? 0),
      );
    }
    throw error;
  }
}

// ─── Agent-name & time-series APIs ───────────────────────────────────────────

/**
 * Fetch distinct agent names observed within the given time range.
 */
export async function fetchAgentNames(
  startNs?: number,
  endNs?: number
): Promise<string[]> {
  const params = new URLSearchParams();
  if (startNs !== undefined) params.set('start_ns', String(startNs));
  if (endNs !== undefined) params.set('end_ns', String(endNs));
  const qs = params.toString() ? `?${params.toString()}` : '';
  return apiFetch<string[]>(`${API_BASE}/api/agent-names${qs}`);
}

export interface TimeseriesBucket {
  bucket_start_ns: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
}

export interface ModelTimeseriesBucket {
  bucket_start_ns: number;
  model: string;
  total_tokens: number;
}

export interface TimeseriesResponse {
  token_series: TimeseriesBucket[];
  model_series: ModelTimeseriesBucket[];
}

export interface MetricPercentiles {
  p50: number;
  p95: number;
  p99: number;
}

export interface LatencyMetricsSummary {
  agent_name: string | null;
  call_count: number;
  streaming_call_count: number;
  ttft_ms: MetricPercentiles | null;
  tps_tokens_per_second: MetricPercentiles | null;
  tpot_ms_per_token: MetricPercentiles | null;
  e2e_latency_ms: MetricPercentiles | null;
}

/**
 * Fetch percentile latency metrics grouped by Agent.
 */
export async function fetchLatencyMetrics(
  startNs: number,
  endNs: number,
  agentName?: string
): Promise<LatencyMetricsSummary[]> {
  const params = new URLSearchParams({
    start_ns: String(startNs),
    end_ns: String(endNs),
  });
  if (agentName) params.set('agent_name', agentName);
  return apiFetch<LatencyMetricsSummary[]>(`${API_BASE}/api/metrics/latency?${params.toString()}`);
}

/**
 * Fetch time-bucketed token stats and per-model breakdowns.
 */
export async function fetchTimeseries(
  startNs: number,
  endNs: number,
  agentName?: string,
  buckets = 30
): Promise<TimeseriesResponse> {
  const params = new URLSearchParams({
    start_ns: String(startNs),
    end_ns: String(endNs),
    buckets: String(buckets),
  });
  if (agentName) params.set('agent_name', agentName);
  return apiFetch<TimeseriesResponse>(`${API_BASE}/api/timeseries?${params.toString()}`);
}

// ─── ATIF export APIs ────────────────────────────────────────────────────────

import type { AtifDocument, AgentHealthResponse, AgentProcessHealthResponse } from '../types';
import type {
  ConfirmState,
  JudgeReport,
  LabelStatsResponse,
  SessionLabelView,
  SessionsResponse,
  TrajectoryLabel,
  TriageReport,
} from '../types/reuse';

// ─── Token Savings types ─────────────────────────────────────────────────────

export interface DiffLine {
  type: 'add' | 'remove' | 'context' | 'separator';
  content: string;
}

export interface OptimizationItem {
  id: string;
  category: 'tool_output' | 'mcp_response';
  title: string;
  strategy: string;
  strategy_label: string;
  before_tokens: number;
  after_tokens: number;
  saved_tokens: number;
  compounded_saved: number;
  compounding_turns: number;
  compression_ratio: number;
  explanation: string;
  before_summary: string;
  after_summary: string;
  optimization_reason: string;
  before_text: string | null;
  after_text: string | null;
  diff_lines: DiffLine[];
}

export interface SessionSavings {
  session_id: string;
  agent_name: string;
  total_input_tokens: number;
  total_output_tokens: number;
  total_tokens: number;
  baseline_tokens: number;
  saved_tokens: number;
  compounded_saved: number;
  /** Fraction of tokens saved, in [0, 1] (not a percentage). */
  savings_rate: number;
  /** Fraction saved including compounding, in [0, 1] (not a percentage). */
  compounded_savings_rate: number;
  request_count: number;
  tool_saved: number;
  mcp_saved: number;
  optimization_items: OptimizationItem[];
}

export interface StrategyBreakdownItem {
  strategy: string;
  label: string;
  saved: number;
  compounded_saved: number;
}

export interface SavingsSummary {
  total_input_tokens: number;
  total_output_tokens: number;
  total_tokens: number;
  baseline_tokens: number;
  total_saved_tokens: number;
  total_compounded_saved: number;
  /** Fraction of tokens saved, in [0, 1] (not a percentage). */
  savings_rate: number;
  /** Fraction saved including compounding, in [0, 1] (not a percentage). */
  compounded_savings_rate: number;
  total_tool_saved: number;
  total_mcp_saved: number;
  total_compounded_tool_saved: number;
  total_compounded_mcp_saved: number;
  strategy_breakdown: StrategyBreakdownItem[];
}

export interface OptimizationTip {
  level: 'success' | 'info' | 'warning';
  title: string;
  description: string;
}

export interface TokenSavingsResponse {
  stats_available: boolean;
  summary: SavingsSummary;
  sessions: SessionSavings[];
  optimization_tips: OptimizationTip[];
}

/**
 * Fetch token savings data within a nanosecond time range.
 */
export async function fetchTokenSavings(
  startNs: number,
  endNs: number,
  agentName?: string,
): Promise<TokenSavingsResponse> {
  const params = new URLSearchParams({
    start_ns: String(startNs),
    end_ns: String(endNs),
  });
  if (agentName) params.set('agent_name', agentName);
  return apiFetch<TokenSavingsResponse>(`${API_BASE}/api/token-savings?${params.toString()}`);
}

// ─── Session-scoped Token Savings ─────────────────────────────────────────────

export interface SessionSavingsDetail {
  session_id: string;
  stats_available: boolean;
  total_actual_tokens: number;
  total_compounded_saved: number;
  total_original_tokens: number;
  /** Fraction of tokens saved, in [0, 1] (not a percentage). */
  savings_rate: number;
  items: OptimizationItem[];
}

/**
 * Fetch token savings detail for a single session.
 */
export async function fetchSessionSavings(
  sessionId: string,
): Promise<SessionSavingsDetail> {
  return apiFetch<SessionSavingsDetail>(
    `${API_BASE}/api/token-savings/session/${encodeURIComponent(sessionId)}`
  );
}

/**
 * Export a single trace as an ATIF trajectory document (shared v1.7 schema).
 */
export async function fetchAtifByTrace(traceId: string): Promise<AtifDocument> {
  return apiFetch<AtifDocument>(
    `${API_BASE}/api/export/atif/trace/${encodeURIComponent(traceId)}`
  );
}

/**
 * Export a full session (all traces) as an ATIF trajectory document (v1.7).
 */
export async function fetchAtifBySession(sessionId: string): Promise<AtifDocument> {
  return apiFetch<AtifDocument>(
    `${API_BASE}/api/export/atif/session/${encodeURIComponent(sessionId)}`
  );
}

/**
 * Export a conversation (all LLM calls for a user query) as an ATIF document (v1.7).
 */
export async function fetchAtifByConversation(conversationId: string): Promise<AtifDocument> {
  return apiFetch<AtifDocument>(
    `${API_BASE}/api/export/atif/conversation/${encodeURIComponent(conversationId)}`
  );
}

/**
 * Read the stored ATIF v1.7 document of a log-collected trajectory.
 * 404s when trajectory collection never ran or the session was not collected.
 */
export async function fetchTrajectoryAtif(sessionId: string): Promise<AtifDocument> {
  return apiFetch<AtifDocument>(
    `${API_BASE}/api/trajectories/${encodeURIComponent(sessionId)}`
  );
}

// ─── Interruption APIs ───────────────────────────────────────────────────────

export type InterruptionSeverity = 'critical' | 'high' | 'medium' | 'low';

export interface InterruptionRecord {
  interruption_id: string;
  session_id: string | null;
  trace_id: string | null;
  conversation_id: string | null;
  call_id: string | null;
  pid: number | null;
  agent_name: string | null;
  interruption_type: string;
  severity: InterruptionSeverity;
  occurred_at_ns: number;
  detail: string | null;
  resolved: boolean;
}

export interface InterruptionCountResponse {
  total: number;
  by_severity: {
    critical: number;
    high: number;
    medium: number;
    low: number;
  };
}

export interface InterruptionTypeStat {
  interruption_type: string;
  severity: string;
  count: number;
}

/**
 * Fetch per-type interruption stats within a time range.
 */
export async function fetchInterruptionStats(
  startNs: number,
  endNs: number
): Promise<InterruptionTypeStat[]> {
  const params = new URLSearchParams();
  params.set('start_ns', String(startNs));
  params.set('end_ns', String(endNs));
  return apiFetch<InterruptionTypeStat[]>(
    `${API_BASE}/api/interruptions/stats?${params.toString()}`
  );
}

/**
 * Fetch interruption events across all sessions within a time range.
 * Omitted params fall back to server defaults (last 24 h, limit 200).
 */
export async function fetchInterruptions(opts?: {
  startNs?: number;
  endNs?: number;
  agentName?: string;
  interruptionType?: string;
  severity?: string;
  resolved?: boolean;
  limit?: number;
}): Promise<InterruptionRecord[]> {
  const params = new URLSearchParams();
  if (opts?.startNs !== undefined) params.set('start_ns', String(opts.startNs));
  if (opts?.endNs !== undefined) params.set('end_ns', String(opts.endNs));
  if (opts?.agentName) params.set('agent_name', opts.agentName);
  if (opts?.interruptionType) params.set('interruption_type', opts.interruptionType);
  if (opts?.severity) params.set('severity', opts.severity);
  if (opts?.resolved !== undefined) params.set('resolved', String(opts.resolved));
  if (opts?.limit !== undefined) params.set('limit', String(opts.limit));
  const qs = params.toString() ? `?${params.toString()}` : '';
  return apiFetch<InterruptionRecord[]>(`${API_BASE}/api/interruptions${qs}`);
}

/** Per-(severity, type) detail returned by session/trace-counts endpoints. */
export interface InterruptionTypeDetail {
  interruption_type: string;
  severity: string;
  count: number;
}

/**
 * Bucket key the backend reports for interruptions whose `session_id` /
 * `conversation_id` could never be resolved.
 *
 * Such events are counted by the overview card, so the per-session breakdown
 * groups them here instead of dropping them — otherwise the total and the
 * breakdown disagree and look like missing data. Must stay in sync with
 * `UNASSIGNED_SESSION_ID` in src/storage/sqlite/interruption.rs.
 */
export const UNASSIGNED_INTERRUPTION_BUCKET = '__unassigned__';

export interface SessionInterruptionCount {
  session_id: string;
  total: number;
  by_severity: {
    critical: number;
    high: number;
    medium: number;
    low: number;
  };
  types: InterruptionTypeDetail[];
}

export interface ConversationInterruptionCount {
  session_id: string;
  conversation_id: string;
  total: number;
  by_severity: {
    critical: number;
    high: number;
    medium: number;
    low: number;
  };
  types: InterruptionTypeDetail[];
}

/**
 * Key for the per-conversation breakdown, which the backend groups by
 * (session_id, conversation_id).
 *
 * Both halves are needed: an interruption detected before its session was
 * resolved carries the unassigned session bucket, and must not be looked up
 * under the real session that owns the same conversation.
 *
 * Invariant: neither id contains U+0000, so the joined key cannot collide.
 * Both are either a UUID, a 32-hex fallback hash, or the `__unassigned__`
 * sentinel — an id carrying a NUL byte would already have failed to round-trip
 * through the SQLite TEXT columns these values come from.
 */
export function conversationInterruptionKey(sessionId: string, conversationId: string): string {
  return `${sessionId}\u0000${conversationId}`;
}

/**
 * Fetch all unresolved interruptions for a session.
 */
export async function fetchSessionInterruptions(sessionId: string): Promise<InterruptionRecord[]> {
  return apiFetch<InterruptionRecord[]>(
    `${API_BASE}/api/sessions/${encodeURIComponent(sessionId)}/interruptions`
  );
}

/**
 * Fetch all unresolved interruptions for a conversation.
 */
export async function fetchConversationInterruptions(conversationId: string): Promise<InterruptionRecord[]> {
  return apiFetch<InterruptionRecord[]>(
    `${API_BASE}/api/conversations/${encodeURIComponent(conversationId)}/interruptions`
  );
}

/**
 * Fetch interruption counts (total + by severity) for the last 24 h.
 */
export async function fetchInterruptionCount(
  startNs?: number,
  endNs?: number,
  agentName?: string
): Promise<InterruptionCountResponse> {
  const params = new URLSearchParams();
  if (startNs !== undefined) params.set('start_ns', String(startNs));
  if (endNs !== undefined) params.set('end_ns', String(endNs));
  if (agentName) params.set('agent_name', agentName);
  const qs = params.toString() ? `?${params.toString()}` : '';
  return apiFetch<InterruptionCountResponse>(`${API_BASE}/api/interruptions/count${qs}`);
}

/**
 * Mark an interruption event as resolved.
 */
export async function resolveInterruption(interruptionId: string): Promise<void> {
  const res = await fetch(
    `${API_BASE}/api/interruptions/${encodeURIComponent(interruptionId)}/resolve`,
    { method: 'POST', credentials: 'same-origin' }
  );
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    throw new Error(`POST /api/interruptions/${interruptionId}/resolve -> ${res.status}: ${text}`);
  }
}

/**
 * Fetch unresolved interruption count + max severity per session_id.
 *
 * `agentName` must match the agent used to filter the session list, otherwise
 * the unassigned bucket would aggregate other agents' interruptions into the
 * filtered table.
 */
export async function fetchInterruptionSessionCounts(
  startNs: number,
  endNs: number,
  agentName?: string
): Promise<SessionInterruptionCount[]> {
  const params = new URLSearchParams();
  params.set('start_ns', String(startNs));
  params.set('end_ns', String(endNs));
  if (agentName) params.set('agent_name', agentName);
  return apiFetch<SessionInterruptionCount[]>(
    `${API_BASE}/api/interruptions/session-counts?${params.toString()}`
  );
}

/**
 * Fetch unresolved interruption count + max severity per conversation_id.
 */
export async function fetchInterruptionConversationCounts(
  startNs: number,
  endNs: number,
  agentName?: string
): Promise<ConversationInterruptionCount[]> {
  const params = new URLSearchParams();
  params.set('start_ns', String(startNs));
  params.set('end_ns', String(endNs));
  if (agentName) params.set('agent_name', agentName);
  return apiFetch<ConversationInterruptionCount[]>(
    `${API_BASE}/api/interruptions/conversation-counts?${params.toString()}`
  );
}

// ─── Agent health API ─────────────────────────────────────────────────────────

/**
 * Fetch historical activity for all Agents observed by either SQLite store.
 */
export async function fetchAgentHealth(): Promise<AgentHealthResponse> {
  return apiFetch<AgentHealthResponse>(`${API_BASE}/api/agent-health`);
}

/** Fetch live process health for diagnostics and restart actions. */
export async function fetchAgentProcessHealth(opts?: {
  includeClients?: boolean;
}): Promise<AgentProcessHealthResponse> {
  const qs = opts?.includeClients ? '?include_clients=true' : '';
  return apiFetch<AgentProcessHealthResponse>(`${API_BASE}/api/agent-process-health${qs}`);
}

/**
 * Acknowledge and remove an offline agent by PID.
 */
export async function deleteAgentHealth(pid: number): Promise<void> {
  const res = await fetch(`${API_BASE}/api/agent-health/${pid}`, { method: 'DELETE', credentials: 'same-origin' });
  if (!res.ok) {
    const text = await res.text().catch(() => res.statusText);
    throw new Error(`DELETE /api/agent-health/${pid} -> ${res.status}: ${text}`);
  }
}

/**
 * Kill and re-launch a hung agent process.
 * Returns the new PID on success.
 */
export async function restartAgentHealth(pid: number): Promise<{ ok: boolean; new_pid: number; cmd: string[] }> {
  const res = await fetch(`${API_BASE}/api/agent-health/${pid}/restart`, { method: 'POST', credentials: 'same-origin' });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error(`POST /api/agent-health/${pid}/restart -> ${res.status}: ${body.error ?? res.statusText}`);
  }
  return body;
}

// ─── Security Observability API ──────────────────────────────────────────────

export type SecurityApiState =
  | 'daemon_reachable'
  | 'disabled'
  | 'daemon_unreachable'
  | 'store_unavailable'
  | 'schema_mismatch'
  | 'ok'
  | 'empty'
  | 'partial'
  | 'found'
  | 'not_found'
  | 'redacted'
  | 'truncated'
  | 'no_correlation'
  | 'low_confidence'
  | 'bad_request'
  | 'payload_too_large'
  | 'timeout'
  | 'busy'
  | 'unavailable'
  | 'error'
  | string;

export interface SecurityApiResponse<T> {
  state: SecurityApiState;
  data: T;
  message?: string;
  meta?: Record<string, unknown>;
}

export interface SecurityRestError {
  code: string;
  message: string;
  retryable?: boolean;
  daemon_code?: string;
}

export class SecurityApiClientError extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryable: boolean;
  readonly daemonCode?: string;

  constructor(status: number, error: SecurityRestError) {
    super(error.message);
    this.name = 'SecurityApiClientError';
    this.status = status;
    this.code = error.code;
    this.retryable = Boolean(error.retryable);
    this.daemonCode = error.daemon_code;
  }
}

export interface SecurityStoreStatus {
  path?: string;
  exists?: boolean;
  ready?: boolean;
  schema_version?: number | null;
  expected_schema_version?: number | null;
  [key: string]: unknown;
}

export interface SecurityStatusData {
  daemon?: Record<string, unknown>;
  stores?: {
    available?: boolean;
    security_db?: SecurityStoreStatus;
    observability_db?: SecurityStoreStatus;
    [key: string]: unknown;
  };
  socket_path?: string | null;
  [key: string]: unknown;
}

export interface SecurityEventRecord {
  event_id: string;
  event_type?: string | null;
  category?: string | null;
  result?: string | null;
  verdict?: string | null;
  command?: string | null;
  skill_name?: string | null;
  timestamp?: string | null;
  timestamp_ns?: number | null;
  timestamp_epoch?: number | null;
  trace_id?: string | null;
  session_id?: string | null;
  run_id?: string | null;
  call_id?: string | null;
  tool_call_id?: string | null;
  pid?: number | null;
  uid?: number | null;
  details?: unknown;
  details_preview?: unknown;
  truncated?: boolean;
  redacted?: boolean;
  [key: string]: unknown;
}

export interface SecuritySummary {
  total: number;
  by_category: Record<string, number>;
  by_event_type: Record<string, number>;
  by_result: Record<string, number>;
  affected_sessions: number;
  affected_runs: number;
  risk_cases_total?: number;
  risk_cases_open?: number;
  risk_cases_blocked?: number;
  latest_events: SecurityEventRecord[];
  [key: string]: unknown;
}

export interface SecurityCountItem {
  value: string | number;
  count: number;
}

export interface SecurityCountByResponse {
  group_by: string;
  items: SecurityCountItem[];
  [key: string]: unknown;
}

export interface SecurityPaginated<T> {
  items: T[];
  total: number;
  limit: number;
  offset: number;
  next_offset?: number | null;
  [key: string]: unknown;
}

export interface SecurityEventDetailResponse {
  found: boolean;
  event: SecurityEventRecord | null;
  [key: string]: unknown;
}

export interface SecuritySessionSummary {
  session_id: string;
  first_seen_ns?: number | null;
  last_seen_ns?: number | null;
  first_seen_epoch?: number | null;
  last_seen_epoch?: number | null;
  turn_count?: number | null;
  observability_event_count?: number | null;
  security_event_count?: number | null;
  highest_severity?: string | null;
  [key: string]: unknown;
}

export interface SecurityRunSummary {
  run_id: string;
  started_at_ns?: number | null;
  ended_at_ns?: number | null;
  started_at_epoch?: number | null;
  ended_at_epoch?: number | null;
  user_input_preview?: string | null;
  observability_event_count?: number | null;
  security_event_count?: number | null;
  [key: string]: unknown;
}

export interface SecurityTimelineObservabilityContext {
  id?: string | number;
  hook?: string | null;
  timestamp?: string | null;
  timestamp_epoch?: number | null;
  session_id?: string | null;
  run_id?: string | null;
  call_id?: string | null;
  tool_call_id?: string | null;
  metadata?: Record<string, unknown>;
  metrics?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface SecurityTimelineItem {
  kind: string;
  id?: string | number;
  hook?: string | null;
  timestamp?: string | null;
  timestamp_ns?: number | null;
  timestamp_epoch?: number | null;
  title?: string | null;
  summary?: string | null;
  session_id?: string | null;
  run_id?: string | null;
  call_id?: string | null;
  tool_call_id?: string | null;
  metadata?: Record<string, unknown>;
  metrics?: Record<string, unknown>;
  observability_event_id?: string | number | null;
  observability?: SecurityTimelineObservabilityContext;
  event?: SecurityEventRecord;
  match?: Record<string, unknown>;
  correlated_security_events?: unknown[];
  truncated?: boolean;
  redacted?: boolean;
  [key: string]: unknown;
}

export interface SecurityTimelineResponse {
  session_id: string;
  run_id: string;
  items: SecurityTimelineItem[];
  [key: string]: unknown;
}

export type SecurityReviewStatus =
  | 'open'
  | 'confirmed'
  | 'false_positive'
  | 'accepted_risk'
  | 'resolved';

export type SecurityRiskSeverity = 'low' | 'medium' | 'high' | 'critical';

export interface SecurityRiskCase {
  case_id: string;
  policy_id: string;
  policy_revision: number;
  agent_id: string;
  session_id?: string | null;
  severity: SecurityRiskSeverity;
  risk_score: number;
  status: SecurityReviewStatus;
  blocked: boolean;
  opened_at_ns: number;
  updated_at_ns: number;
  summary: string;
}

export interface SecurityEvidenceEvent {
  event_id: string;
  event_type: string;
  occurred_at_ns: number;
  identity: {
    pid: number;
    ppid?: number | null;
    session_id?: string | null;
    tool_call_id?: string | null;
    [key: string]: unknown;
  };
  event: Record<string, unknown>;
  [key: string]: unknown;
}

export interface SecurityRiskCaseDetail extends SecurityRiskCase {
  evidence: SecurityEvidenceEvent[];
  containment: SecurityContainmentAction | null;
}

export type SecurityContainmentLifecycle =
  | 'pending'
  | 'active'
  | 'expiring'
  | 'expired'
  | 'failed';

export interface SecurityContainmentCandidate {
  agent_id: string;
  root_pid: number;
  process_start_time: number;
  display_name: string;
}

export interface SecurityContainmentAction {
  action_id: string;
  case_id: string;
  binding_id: string;
  agent_id: string;
  root_pid: number;
  process_start_time: number;
  duration_secs: number | null;
  expires_at_ns: number | null;
  lifecycle_state: SecurityContainmentLifecycle;
  blocked_at_ns: number | null;
  requested_by: string;
  failure_stage: 'attach' | 'detach' | 'reconcile' | null;
  failure_summary: string | null;
  attempt_count: number;
  next_retry_at_ns: number | null;
  created_at_ns: number;
  updated_at_ns: number;
}

export interface SecurityContainmentPlan {
  case_id: string;
  source_path: string;
  original_target: SecurityContainmentCandidate | null;
  original_target_valid: boolean;
  candidates: SecurityContainmentCandidate[];
  default_duration_secs: number;
  min_duration_secs: number;
  max_duration_secs: number;
  existing_action: SecurityContainmentAction | null;
}

export function containmentTargetCandidates(
  plan: SecurityContainmentPlan,
  health: EnforcementHealth | null,
): SecurityContainmentCandidate[] {
  if (plan.original_target_valid && plan.original_target) {
    return [plan.original_target];
  }
  if (
    health?.ready !== true
    || health.capabilities.alternate_pid_retarget !== true
  ) {
    return [];
  }
  return plan.candidates;
}

export function defaultContainmentTargetPid(
  plan: SecurityContainmentPlan,
  health: EnforcementHealth | null,
): number | null {
  const candidates = containmentTargetCandidates(plan, health);
  return candidates.length === 1 ? candidates[0].root_pid : null;
}

export interface SecurityContainmentRequest {
  root_pid: number;
  duration_secs: number | null;
}

export type SecurityQueryValue = string | number | boolean | null | undefined;

export interface SecurityTimeRangeParams {
  start_ns?: number;
  end_ns?: number;
}

export interface SecurityEventListParams extends SecurityTimeRangeParams {
  event_type?: string;
  category?: string;
  result?: string;
  verdict?: string;
  trace_id?: string;
  session_id?: string;
  run_id?: string;
  call_id?: string;
  tool_call_id?: string;
  limit?: number;
  offset?: number;
  include_details?: boolean;
}

export interface SecuritySessionListParams extends SecurityTimeRangeParams {
  workspace_id?: string;
  limit?: number;
  offset?: number;
}

export interface SecurityRunListParams extends SecurityTimeRangeParams {
  limit?: number;
  offset?: number;
}

export interface SecurityCountByParams extends SecurityTimeRangeParams {
  event_type?: string;
  category?: string;
  result?: string;
  verdict?: string;
  trace_id?: string;
  session_id?: string;
  run_id?: string;
  call_id?: string;
  tool_call_id?: string;
}

export interface SecurityTimelineParams extends SecurityTimeRangeParams {
  session_id: string;
  run_id: string;
  limit?: number;
  include_security?: boolean;
}

function buildQuery(params?: object): string {
  if (!params) return '';
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params) as [string, SecurityQueryValue][]) {
    if (value === undefined || value === null || value === '') continue;
    query.set(key, String(value));
  }
  const qs = query.toString();
  return qs ? `?${qs}` : '';
}

export async function securityFetch<T>(url: string): Promise<SecurityApiResponse<T>> {
  return fetchSecurityApi<T>(url, false);
}

async function auditFetch<T>(url: string): Promise<SecurityApiResponse<T>> {
  return fetchSecurityApi<T>(url, true);
}

async function fetchSecurityApi<T>(
  url: string,
  rejectHttpErrorBeforeState: boolean,
): Promise<SecurityApiResponse<T>> {
  const res = await fetch(url, { credentials: 'same-origin' });
  if (res.status === 401) {
    window.location.hash = '#/login';
    throw new Error('Authentication required');
  }
  const text = await res.text().catch(() => '');
  let body: unknown = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }

  if (!res.ok && rejectHttpErrorBeforeState) {
    const errorBody = body && typeof body === 'object' && 'error' in body
      ? (body as { error: SecurityRestError }).error
      : {
          code: 'security_api_error',
          message: typeof body === 'string' && body ? body : res.statusText,
          retryable: false,
        };
    throw new SecurityApiClientError(res.status, errorBody);
  }

  if (body && typeof body === 'object' && 'state' in body) {
    const envelope = body as Record<string, unknown>;
    if (typeof envelope.state !== 'string' || !isObjectRecord(envelope.data)) {
      throw new SecurityApiClientError(res.status, {
        code: 'malformed_security_response',
        message: 'Security API returned a malformed state response',
        retryable: false,
      });
    }
    return body as SecurityApiResponse<T>;
  }

  if (!res.ok) {
    const errorBody = body && typeof body === 'object' && 'error' in body
      ? (body as { error: SecurityRestError }).error
      : {
          code: 'security_api_error',
          message: typeof body === 'string' && body ? body : res.statusText,
          retryable: false,
        };
    throw new SecurityApiClientError(res.status, errorBody);
  }

  return {
    state: 'ok',
    data: body as T,
    meta: { source: 'agentsight' },
  };
}

function isObjectRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export async function fetchSecurityStatus(): Promise<SecurityApiResponse<SecurityStatusData>> {
  return securityFetch<SecurityStatusData>(`${API_BASE}/api/security/status`);
}

export async function fetchSecuritySummary(
  params?: SecurityTimeRangeParams & { latest_limit?: number },
): Promise<SecurityApiResponse<SecuritySummary>> {
  return securityFetch<SecuritySummary>(
    `${API_BASE}/api/security/summary${buildQuery(params)}`
  );
}

export async function fetchSecurityEvents(
  params?: SecurityEventListParams,
): Promise<SecurityApiResponse<SecurityPaginated<SecurityEventRecord>>> {
  return securityFetch<SecurityPaginated<SecurityEventRecord>>(
    `${API_BASE}/api/security/events${buildQuery(params)}`
  );
}

export async function fetchSecurityEvent(
  eventId: string,
): Promise<SecurityApiResponse<SecurityEventDetailResponse>> {
  return securityFetch<SecurityEventDetailResponse>(
    `${API_BASE}/api/security/events/${encodeURIComponent(eventId)}`
  );
}

export async function fetchSecurityCountBy(
  groupBy: string,
  params?: SecurityCountByParams,
): Promise<SecurityApiResponse<SecurityCountByResponse>> {
  return securityFetch<SecurityCountByResponse>(
    `${API_BASE}/api/security/events/count-by${buildQuery({ ...params, group_by: groupBy })}`
  );
}

export async function fetchSecuritySessions(
  params?: SecuritySessionListParams,
): Promise<SecurityApiResponse<SecurityPaginated<SecuritySessionSummary>>> {
  return securityFetch<SecurityPaginated<SecuritySessionSummary>>(
    `${API_BASE}/api/security/observability/sessions${buildQuery(params)}`
  );
}

export async function fetchSecurityRuns(
  sessionId: string,
  params?: SecurityRunListParams,
): Promise<SecurityApiResponse<SecurityPaginated<SecurityRunSummary>>> {
  return securityFetch<SecurityPaginated<SecurityRunSummary>>(
    `${API_BASE}/api/security/observability/sessions/${encodeURIComponent(sessionId)}/runs${buildQuery(params)}`
  );
}

export async function fetchSecurityTimeline(
  params: SecurityTimelineParams,
): Promise<SecurityApiResponse<SecurityTimelineResponse>> {
  const { session_id, run_id, ...queryParams } = params;
  return securityFetch<SecurityTimelineResponse>(
    `${API_BASE}/api/security/observability/timeline${buildQuery({
      ...queryParams,
      session_id,
      run_id,
    })}`
  );
}

export async function fetchAuditSummary(
  params?: SecurityTimeRangeParams & { limit?: number },
): Promise<SecurityApiResponse<SecuritySummary>> {
  return auditFetch<SecuritySummary>(
    `${API_BASE}/api/audit/summary${buildQuery(params)}`,
  );
}

export async function fetchAuditEvents(
  params?: SecurityEventListParams,
): Promise<SecurityApiResponse<SecurityPaginated<SecurityEventRecord>>> {
  return auditFetch<SecurityPaginated<SecurityEventRecord>>(
    `${API_BASE}/api/audit/events${buildQuery(params)}`,
  );
}

export async function fetchAuditSessions(
  params?: SecuritySessionListParams,
): Promise<SecurityApiResponse<SecurityPaginated<SecuritySessionSummary>>> {
  return auditFetch<SecurityPaginated<SecuritySessionSummary>>(
    `${API_BASE}/api/audit/sessions${buildQuery(params)}`,
  );
}

export async function fetchSecurityCases(
  params?: { limit?: number; offset?: number; agent_id?: string; status?: string; blocked?: boolean },
): Promise<SecurityApiResponse<SecurityPaginated<SecurityRiskCase>>> {
  return auditFetch<SecurityPaginated<SecurityRiskCase>>(
    `${API_BASE}/api/audit/cases${buildQuery(params)}`,
  );
}

export async function fetchSecurityCase(
  caseId: string,
): Promise<SecurityApiResponse<SecurityRiskCaseDetail>> {
  const response = await auditFetch<SecurityRiskCaseDetail>(
    `${API_BASE}/api/audit/cases/${encodeURIComponent(caseId)}`,
  );
  if (!isSecurityRiskCaseDetail(response.data)) {
    throw new SecurityApiClientError(200, {
      code: 'malformed_security_case',
      message: 'Security API returned a malformed case detail',
      retryable: false,
    });
  }
  return response;
}

function isSecurityRiskCaseDetail(value: unknown): value is SecurityRiskCaseDetail {
  if (!isObjectRecord(value) || !Array.isArray(value.evidence)) return false;
  const hasCaseFields = [
    'case_id',
    'policy_id',
    'agent_id',
    'severity',
    'status',
    'summary',
  ].every((field) => typeof value[field] === 'string')
    && typeof value.policy_revision === 'number'
    && typeof value.risk_score === 'number'
    && typeof value.blocked === 'boolean'
    && typeof value.opened_at_ns === 'number'
    && typeof value.updated_at_ns === 'number'
    && (value.containment === null || isObjectRecord(value.containment));
  return hasCaseFields && value.evidence.every((event) => (
    isObjectRecord(event)
      && typeof event.event_id === 'string'
      && typeof event.event_type === 'string'
      && typeof event.occurred_at_ns === 'number'
      && isObjectRecord(event.identity)
      && isObjectRecord(event.event)
  ));
}

export async function fetchContainmentPlan(
  caseId: string,
): Promise<SecurityApiResponse<SecurityContainmentPlan>> {
  return auditFetch<SecurityContainmentPlan>(
    `${API_BASE}/api/audit/cases/${encodeURIComponent(caseId)}/containment-plan`,
  );
}

export async function containSecurityCase(
  caseId: string,
  request: SecurityContainmentRequest,
): Promise<SecurityApiResponse<SecurityContainmentAction>> {
  const response = await fetch(
    `${API_BASE}/api/audit/cases/${encodeURIComponent(caseId)}/contain`,
    {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(request),
    },
  );
  if (response.status === 401) {
    window.location.hash = '#/login';
    throw new Error('Authentication required');
  }
  const body = await response.json().catch(() => null) as
    | SecurityApiResponse<SecurityContainmentAction>
    | { error?: SecurityRestError }
    | null;
  if (!response.ok || !body || !('state' in body)) {
    const error = body && 'error' in body ? body.error : undefined;
    throw new SecurityApiClientError(response.status, error ?? {
      code: 'containment_request_failed',
      message: response.statusText || 'Containment request failed',
      retryable: false,
    });
  }
  return body;
}

export async function reviewSecurityCase(
  caseId: string,
  status: Exclude<SecurityReviewStatus, 'open'>,
): Promise<SecurityApiResponse<SecurityRiskCase>> {
  const response = await fetch(
    `${API_BASE}/api/audit/cases/${encodeURIComponent(caseId)}/review`,
    {
      method: 'POST',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ status }),
    },
  );
  if (response.status === 401) {
    window.location.hash = '#/login';
    throw new Error('Authentication required');
  }
  const body = await response.json().catch(() => null) as SecurityApiResponse<SecurityRiskCase> | {
    error?: SecurityRestError;
  } | null;
  if (!response.ok || !body || !('state' in body)) {
    const error = body && 'error' in body ? body.error : undefined;
    throw new SecurityApiClientError(response.status, error ?? {
      code: 'security_review_failed',
      message: response.statusText || 'Risk case review failed',
      retryable: false,
    });
  }
  return body;
}

// ─── Skill Metrics types ──────────────────────────────────────────────────────

export interface SkillFirstSeen {
  first_seen_session_id: string;
  first_seen_timestamp_ns: number;
  total_sessions: number;
}

export interface SkillDownloadMetrics {
  downloads: Record<string, SkillFirstSeen>;
}

export interface SkillLoadMetrics {
  loads: Record<string, number>;
  total_loads: number;
}

export interface SkillUsageRatio {
  ratio: number;
  with_skill_count: number;
  without_skill_count: number;
  total_sessions: number;
}

export interface SkillCountDistribution {
  min: number;
  max: number;
  mean: number;
  median: number;
  p90: number;
  histogram: [number, number, number, number, number, number];
}

export interface WeeklyRank {
  iso_week: string;
  load_count: number;
  rank: number;
}

export interface SkillRankEntry {
  skill_name: string;
  total_loads: number;
  total_rank: number;
  weekly_ranks: WeeklyRank[];
  rank_delta: number | null;
}

export interface SkillHotnessRanking {
  rankings: SkillRankEntry[];
}

export interface SkillMetricsReport {
  downloads: SkillDownloadMetrics | null;
  loads: SkillLoadMetrics | null;
  usage_ratio: SkillUsageRatio | null;
  distribution: SkillCountDistribution | null;
  hotness: SkillHotnessRanking | null;
  computed_at: string;
  time_range_ns: [number, number];
  event_count: number;
}

// ─── Skill Metrics API ────────────────────────────────────────────────────────

function buildSkillMetricsParams(startNs?: number, endNs?: number, agentName?: string, granularity?: string): string {
  const params = new URLSearchParams();
  if (startNs !== undefined) params.set('start_ns', String(startNs));
  if (endNs !== undefined) params.set('end_ns', String(endNs));
  if (agentName) params.set('agent_name', agentName);
  if (granularity) params.set('granularity', granularity);
  const qs = params.toString();
  return qs ? `?${qs}` : '';
}

export async function fetchSkillMetrics(
  startNs?: number,
  endNs?: number,
  agentName?: string,
  granularity?: string,
): Promise<SkillMetricsReport> {
  return apiFetch<SkillMetricsReport>(
    `${API_BASE}/api/skill-metrics${buildSkillMetricsParams(startNs, endNs, agentName, granularity)}`
  );
}

// ─── Authentication API ──────────────────────────────────────────────────────

export type AppCapability =
  | 'agent_observability'
  | 'sessions'
  | 'reuse_labels'
  | 'token_savings'
  | 'optimization'
  | 'skills'
  | 'security'
  | 'system_audit'
  | 'enforcement'
  | 'atif'
  | 'settings'
  | 'agent_health';

export interface AuthStatusResponse {
  auth_enabled: boolean;
  mode?: 'linux' | 'local' | string;
  capabilities?: AppCapability[];
}

export interface AuthVerifyResponse {
  authenticated: boolean;
}

/**
 * Check whether dashboard authentication is enabled.
 */
export async function fetchAuthStatus(): Promise<AuthStatusResponse> {
  const res = await fetch(`${API_BASE}/api/auth/status`);
  if (!res.ok) throw new Error(`GET /api/auth/status -> ${res.status}`);
  return res.json();
}

/**
 * Verify the current session (cookie-based).
 */
export async function fetchAuthVerify(): Promise<AuthVerifyResponse> {
  const res = await fetch(`${API_BASE}/api/auth/verify`, { credentials: 'same-origin' });
  if (!res.ok) throw new Error(`GET /api/auth/verify -> ${res.status}`);
  return res.json();
}

/**
 * Log in with a token string. Sets a session cookie on success.
 */
export async function login(token: string): Promise<boolean> {
  const res = await fetch(`${API_BASE}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify({ token }),
  });
  return res.ok;
}

// ─── Storage status API ─────────────────────────────────────────────────────

export type StorageAvailability = 'present' | 'missing' | 'error' | 'external';
export type StorageCoverage = 'full' | 'row_bounded' | 'unmanaged' | 'external';
export type StorageSizeState =
  | 'within_policy'
  | 'cleanup_due'
  | 'reusable_capacity'
  | 'disabled'
  | 'unknown';

export interface StorageSizeStatus {
  database_bytes: number;
  wal_bytes: number;
  shm_bytes: number;
  freelist_bytes: number;
  physical_bytes: number;
  logical_bytes: number;
}

export interface StoragePolicyStatus {
  retention_days: number;
  size_limit_bytes: number;
  cleanup_trigger_bytes: number;
  cleanup_target_bytes: number;
  check_interval: number;
  check_interval_unit: 'inserts' | 'seconds' | 'none' | 'external' | string;
  enforced_by: string;
}

export interface StorageStoreStatus {
  id: string;
  availability: StorageAvailability;
  size: StorageSizeStatus | null;
  policy: StoragePolicyStatus;
  coverage: StorageCoverage;
  size_state: StorageSizeState;
}

export interface StorageStatusResponse {
  schema_version: number;
  observed_at_unix_ms: number;
  stores: StorageStoreStatus[];
}

/** Read effective SQLite policies and current disk allocation. */
export async function fetchStorageStatus(): Promise<StorageStatusResponse> {
  return apiFetch<StorageStatusResponse>(`${API_BASE}/api/storage/status`);
}

// ─── Optimization analysis API ───────────────────────────────────────────────

export type OptimizeDimension =
  | 'perf'
  | 'perf-issues'
  | 'cost'
  | 'cost-waste'
  | 'accuracy'
  | 'summary';

/** Dimension keys as persisted in optimization.db (underscored, unlike the route form). */
export type OptimizeHistoryDimension =
  | 'perf'
  | 'perf_issues'
  | 'cost'
  | 'cost_waste'
  | 'accuracy'
  | 'summary';

/** One row of the analysis history list — presence flags only, no payloads. */
export interface OptimizeHistoryEntry {
  session_id: string;
  dimensions: OptimizeHistoryDimension[];
  created_at_ns: number;
  updated_at_ns: number;
}

/**
 * List previously analyzed sessions, newest first. The server defaults to the
 * last 30 days and caps `limit` at 200.
 */
export async function fetchOptimizeHistory(limit?: number): Promise<OptimizeHistoryEntry[]> {
  const qs = limit === undefined ? '' : `?limit=${limit}`;
  return apiFetch<OptimizeHistoryEntry[]>(`${API_BASE}/api/optimize/results${qs}`);
}

/** Load persisted analysis results for a session (each dimension may be null). */
export async function fetchOptimizeResults(sessionId: string): Promise<OptimizeSessionResults> {
  return apiFetch<OptimizeSessionResults>(
    `${API_BASE}/api/optimize/sessions/${encodeURIComponent(sessionId)}/results`
  );
}

/**
 * Trigger one analysis dimension for a session.
 * perf/cost return in milliseconds; perf-issues/cost-waste take 10-30s;
 * accuracy can take 30-60s+ — callers must not set a short timeout.
 */
export async function runOptimizeDimension<T>(
  sessionId: string,
  dimension: OptimizeDimension
): Promise<T> {
  return apiFetch<T>(
    `${API_BASE}/api/optimize/sessions/${encodeURIComponent(sessionId)}/${dimension}`,
    { method: 'POST' }
  );
}

/** Read the current LLM config (api_key is masked by the backend). */
export async function fetchOptimizeConfig(): Promise<OptimizeLlmConfig> {
  return apiFetch<OptimizeLlmConfig>(`${API_BASE}/api/optimize/config`);
}

/**
 * Save the LLM config. Omitted/empty fields keep their current value;
 * an api_key containing masking dots (•) is ignored by the backend.
 */
export async function saveOptimizeConfig(body: {
  api_key?: string;
  base_url?: string;
  model?: string;
}): Promise<OptimizeLlmConfig> {
  return apiFetch<OptimizeLlmConfig>(`${API_BASE}/api/optimize/config`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

/** Run causal attribution on a trajectory and return the causal case. */
export async function runCausalAttribution(
  body: import('../types/causal').CausalAttributionRequest,
): Promise<import('../types/causal').CausalAttributionResponse> {
  return apiFetch<import('../types/causal').CausalAttributionResponse>(
    `${API_BASE}/api/causal-attribution`,
    {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body),
    },
  );
}
