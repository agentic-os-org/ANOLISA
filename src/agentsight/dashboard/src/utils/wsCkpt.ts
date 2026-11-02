import type { AtifDocument } from '../types';
import { API_BASE, apiFetch } from './apiClient';

export interface WorkspaceSnapshot {
  id: string;
  workspace: string;
  meta: { message?: string | null; created_at: string; missing?: boolean };
}

export interface SnapshotScope {
  ws_id: string;
  path: string;
  snapshot_count: number;
}

export interface SnapshotBatch {
  trajectory: AtifDocument;
  scope: SnapshotScope | null;
  association?: { revision: string; as_of: string };
  step_snapshots: Record<string, WorkspaceSnapshot[]>;
  completeness?: { complete: boolean; next_cursor: string | null };
  restore_enabled?: boolean;
}

export interface FileChange {
  path: string;
  change_type: 'Added' | 'Modified' | 'Deleted' | 'Renamed';
  detail: string | null;
}

export interface SnapshotTarget {
  workspace: string;
  snapshot_id: string;
}

export interface RestorePreview {
  generation: number[];
  diff_digest: number[];
}

export interface SnapshotDiff {
  changes: FileChange[];
  scope: SnapshotScope;
  snapshot_id: string;
  compared_at: string;
  restore_preview?: RestorePreview;
}

export interface RestoreConfirmation {
  confirmation_token: string;
  operation_id: string;
}

export interface RestoreResult {
  evidence: { outcome: 'Started' | { Unknown: { reason: string } } | { Succeeded: unknown } } | null;
}

const endpoint = `${API_BASE}/api/ws-ckpt`;
function snapshotUrl(kind: 'session' | 'conversation', id: string): string {
  return `${endpoint}/${kind}/${encodeURIComponent(id)}/snapshots`;
}

export async function canUnlockWorkspaceSnapshots(kind: 'session' | 'conversation', id: string, signal?: AbortSignal): Promise<boolean> {
  const response = await fetch(snapshotUrl(kind, id), { method: 'HEAD', cache: 'no-store', signal }).catch(() => null);
  return response?.status === 204;
}

function post<T>(path: string, body: unknown, redirectOnUnauthorized = true, signal?: AbortSignal): Promise<T> {
  return apiFetch<T>(`${endpoint}/${path}`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body), signal,
  }, redirectOnUnauthorized);
}

/** Read every page before attaching rows; early pages may contain only unassigned snapshots. */
export async function fetchWorkspaceSnapshots(
  kind: 'session' | 'conversation', id: string, signal?: AbortSignal,
): Promise<SnapshotBatch> {
  const url = snapshotUrl(kind, id);
  // Let the viewer check workspace availability before redirecting a loopback 401.
  const batch = await apiFetch<SnapshotBatch>(url, { signal }, false);
  let cursor = batch.completeness?.next_cursor;
  while (cursor) {
    const page = await apiFetch<SnapshotBatch>(`${url}?cursor=${encodeURIComponent(cursor)}`, { signal }, false);
    if (page.association?.revision !== batch.association?.revision
      || page.association?.as_of !== batch.association?.as_of) {
      throw new Error('Snapshot page changed; reload the trajectory');
    }
    for (const [stepId, rows] of Object.entries(page.step_snapshots)) {
      batch.step_snapshots[stepId] = [...(batch.step_snapshots[stepId] ?? []), ...rows];
    }
    batch.completeness = page.completeness;
    cursor = page.completeness?.next_cursor;
  }
  for (const rows of Object.values(batch.step_snapshots)) {
    rows.sort((a, b) => a.meta.created_at.localeCompare(b.meta.created_at) || a.id.localeCompare(b.id));
  }
  return batch;
}

type JsonRecord = Record<string, unknown>;

const isRecord = (value: unknown): value is JsonRecord =>
  typeof value === 'object' && value !== null && !Array.isArray(value);

/** Return a JSON value with object keys sorted, without changing array order or values. */
function canonicalJson(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(canonicalJson);
  if (!isRecord(value)) return value;
  return Object.fromEntries(Object.entries(value)
    .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
    .map(([key, child]) => [key, canonicalJson(child)]));
}

function jsonText(value: unknown): string {
  return JSON.stringify(canonicalJson(value));
}

function copyOptional(source: JsonRecord, target: JsonRecord, key: string, normalize = canonicalJson): void {
  // Rust serializes Option<T> as an omitted field for both a missing and null input.
  if (source[key] != null) target[key] = normalize(source[key]);
}

function normalizeToolCall(value: unknown): unknown {
  if (!isRecord(value)) return canonicalJson(value);
  const normalized: JsonRecord = {
    tool_call_id: canonicalJson(value.tool_call_id),
    function_name: canonicalJson(value.function_name),
    // ToolCall.arguments is #[serde(default)]: absent becomes JSON null, not {}.
    arguments: canonicalJson(value.arguments === undefined ? null : value.arguments),
  };
  copyOptional(value, normalized, 'extra');
  return normalized;
}

function normalizeSubagentRef(value: unknown): unknown {
  if (!isRecord(value)) return canonicalJson(value);
  const normalized: JsonRecord = {};
  for (const key of ['trajectory_id', 'trajectory_path', 'session_id', 'extra']) {
    copyOptional(value, normalized, key);
  }
  return normalized;
}

function normalizeObservationResult(value: unknown): unknown {
  if (!isRecord(value)) return canonicalJson(value);
  const normalized: JsonRecord = {};
  copyOptional(value, normalized, 'source_call_id');
  copyOptional(value, normalized, 'content');
  copyOptional(value, normalized, 'subagent_trajectory_ref', (refs) =>
    Array.isArray(refs) ? refs.map(normalizeSubagentRef) : canonicalJson(refs));
  copyOptional(value, normalized, 'extra');
  return normalized;
}

function normalizeObservation(value: unknown): unknown {
  if (!isRecord(value)) return canonicalJson(value);
  // Observation.results is #[serde(default)]: only an omitted field defaults to [].
  const results = value.results;
  return {
    results: Array.isArray(results)
      ? results.map(normalizeObservationResult)
      : results === undefined ? [] : canonicalJson(results),
  };
}

function normalizeMetrics(value: unknown): unknown {
  if (!isRecord(value)) return canonicalJson(value);
  const normalized: JsonRecord = {};
  for (const key of [
    'prompt_tokens', 'completion_tokens', 'cached_tokens', 'cost_usd', 'logprobs',
    'completion_token_ids', 'prompt_token_ids', 'extra',
  ]) {
    copyOptional(value, normalized, key);
  }
  return normalized;
}

/**
 * Match the Step JSON Rust emits after parsing agentsight-atif input.
 *
 * The snapshot endpoint performs that parse/serialize round trip, while a
 * collected trajectory can still be returned in its raw form. This only
 * normalizes documented serde defaults and Option omission; arbitrary payloads
 * such as tool arguments, result content, and `extra` stay intact.
 */
function normalizeStep(value: unknown): unknown {
  if (!isRecord(value)) return canonicalJson(value);
  const message = value.message;
  const normalized: JsonRecord = {
    step_id: canonicalJson(value.step_id),
    source: canonicalJson(value.source),
    // de_message flattens structured messages with their original key order; null becomes empty text.
    message: message == null ? '' : typeof message === 'string' ? message : JSON.stringify(message),
  };
  for (const key of [
    'timestamp', 'model_name', 'reasoning_effort', 'reasoning_content',
    'llm_call_count', 'is_copied_context', 'extra',
  ]) {
    copyOptional(value, normalized, key);
  }
  copyOptional(value, normalized, 'tool_calls', (calls) =>
    Array.isArray(calls) ? calls.map(normalizeToolCall) : canonicalJson(calls));
  copyOptional(value, normalized, 'observation', normalizeObservation);
  copyOptional(value, normalized, 'metrics', normalizeMetrics);
  return normalized;
}

function normalizeSteps(steps: unknown): unknown {
  // AtifTrajectory.steps is #[serde(default)]; null is invalid and deliberately
  // not treated as an empty trajectory.
  if (steps === undefined) return [];
  return Array.isArray(steps) ? steps.map(normalizeStep) : canonicalJson(steps);
}

/** Never attach root Step IDs to an imported, updated, or embedded child trajectory. */
export function snapshotsMatchDocument(batch: SnapshotBatch, doc: AtifDocument): boolean {
  return jsonText(normalizeSteps(batch.trajectory.steps)) === jsonText(normalizeSteps(doc.steps));
}

export const fetchSnapshotDiff = (target: SnapshotTarget) => post<SnapshotDiff>('diff', target);
export const prepareSnapshotRestore = (target: SnapshotTarget, expected_preview: RestorePreview) =>
  post<RestoreConfirmation>('restore/prepare', { target, writers_stopped: true, expected_preview });
export const executeSnapshotRestore = (confirmation_token: string) =>
  post<{ status: 'succeeded' | 'unknown' }>('restore/execute', { confirmation_token });
export const fetchSnapshotRestoreResult = (confirmation_token: string, signal?: AbortSignal) =>
  post<RestoreResult>('restore/result', { confirmation_token }, false, signal);
