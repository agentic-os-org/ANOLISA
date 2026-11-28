import type { SessionSummary, TrajectorySummary } from './apiClient';

export type SessionSource = 'ebpf' | 'log';

/** One row in the unified session list, merged from both data sources. */
export interface MergedSession {
  session_id: string;
  sources: SessionSource[];
  agent_name: string | null;
  project: string | null;
  model: string | null;
  /** eBPF conversation count or log-collected step count. */
  count: number;
  input_tokens: number | null;
  output_tokens: number | null;
  /** First user message preview (≤ 200 chars). */
  first_message: string | null;
  /** Latest user message preview (≤ 200 chars). */
  last_message: string | null;
  /** Last activity in epoch milliseconds (null when unknown). */
  last_active_ms: number | null;
  /** Number of subagent trajectories spawned by this session. */
  subagent_count: number;
}

/** Trailing 36-char UUID of a Codex rollout stem (`rollout-<ts>-<uuid>`). */
const TRAILING_UUID_RE =
  /^rollout-.+-([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/;

/**
 * Merge eBPF-captured sessions with log-collected trajectories by session_id.
 * Qoder sessions share the same UUID across both sources; on overlap, tokens /
 * model / count come from the eBPF side and project / source from the log side.
 */
export function mergeSessions(
  ebpf: SessionSummary[] = [],
  logs: TrajectorySummary[] = [],
): MergedSession[] {
  const ebpfRows = Array.isArray(ebpf) ? ebpf : [];
  const logRows = Array.isArray(logs) ? logs : [];
  const logSessionIds = new Set(logRows.map((row) => row.session_id));
  const byId = new Map<string, MergedSession>();
  const lastPreviewMsById = new Map<string, number | null>();

  // Count subagents per parent session. A subagent's session_id follows the
  // composite form "<parent>:subagent:<child>"; tally by the parent prefix.
  // A Codex parent captured only by eBPF is keyed by the bare trailing UUID
  // while its children carry the full rollout stem, so tally under that alias
  // too (same alias the row merge below uses) — an eBPF-only parent row must
  // still show its child count.
  const subagentCount = new Map<string, number>();
  for (const t of logRows) {
    if (!t.is_subagent) continue;
    const idx = t.session_id.lastIndexOf(':subagent:');
    if (idx > 0) {
      const parent = t.session_id.slice(0, idx);
      subagentCount.set(parent, (subagentCount.get(parent) ?? 0) + 1);
      const parentUuid = TRAILING_UUID_RE.exec(parent)?.[1];
      if (parentUuid && parentUuid !== parent) {
        subagentCount.set(parentUuid, (subagentCount.get(parentUuid) ?? 0) + 1);
      }
    }
  }

  for (const s of ebpfRows) {
    const lastSeenMs = s.last_seen_ns > 0 ? Math.floor(s.last_seen_ns / 1_000_000) : null;
    lastPreviewMsById.set(s.session_id, s.last_user_query?.trim() ? lastSeenMs : null);
    byId.set(s.session_id, {
      session_id: s.session_id,
      sources: ['ebpf'],
      agent_name: s.agent_name,
      project: null,
      model: s.model,
      count: s.conversation_count,
      input_tokens: s.total_input_tokens,
      output_tokens: s.total_output_tokens,
      first_message: s.first_user_query ?? null,
      last_message: s.last_user_query ?? null,
      last_active_ms: lastSeenMs,
      subagent_count: subagentCount.get(s.session_id) ?? 0,
    });
  }

  for (const t of logRows) {
    // Subagent trajectories belong to their parent task — don't show them as
    // independent rows. They remain reachable via the parent's ATIF viewer
    // (breadcrumb navigation). Only keep orphaned subagents whose parent is
    // absent from both sources (e.g. parent outside the selected time range).
    if (t.is_subagent) {
      const idx = t.session_id.lastIndexOf(':subagent:');
      const parentId = idx > 0 ? t.session_id.slice(0, idx) : null;
      // Resolve the parent through the trailing-UUID alias as well: a Codex
      // parent captured only by eBPF sits in `byId` under the bare UUID while
      // the child's composite id carries the full rollout stem. Without the
      // alias the child survives as a stray orphan row next to its parent.
      const parentUuid = parentId ? TRAILING_UUID_RE.exec(parentId)?.[1] : undefined;
      const parentKnown =
        !!parentId &&
        (byId.has(parentId) ||
          logSessionIds.has(parentId) ||
          (!!parentUuid &&
            parentUuid !== parentId &&
            (byId.has(parentUuid) || logSessionIds.has(parentUuid))));
      if (parentKnown) {
        continue;
      }
    }
    const parsedEnd = Date.parse(t.end_time ?? '');
    const endMs = Number.isFinite(parsedEnd) ? parsedEnd : null;
    const lastMs = endMs ?? (t.collected_at_ns > 0 ? Math.floor(t.collected_at_ns / 1_000_000) : null);
    // Codex rollout trajectories keep the full file stem (`rollout-<ts>-<uuid>`)
    // as session_id while the eBPF side reports the bare trailing UUID; fall
    // back to it so a session captured by both paths is merged, not duplicated.
    const uuid = TRAILING_UUID_RE.exec(t.session_id)?.[1];
    const existing =
      byId.get(t.session_id) ?? (uuid && uuid !== t.session_id ? byId.get(uuid) : undefined);
    if (existing) {
      existing.sources.push('log');
      existing.project = t.project || existing.project;
      existing.agent_name = existing.agent_name || t.agent_name;
      existing.model = existing.model || t.model_name;
      // The complete log remains the preferred source for the first prompt.
      existing.first_message = t.first_user_message ?? existing.first_message;
      // Collection time can be newer than the prompt it read. Compare actual
      // source activity, and keep the chosen preview's time across ID aliases.
      const previousPreviewMs = lastPreviewMsById.get(existing.session_id);
      const hasNewerPreview = previousPreviewMs != null && endMs !== null && previousPreviewMs > endMs;
      if (t.last_user_message != null && !hasNewerPreview) {
        existing.last_message = t.last_user_message;
        lastPreviewMsById.set(existing.session_id, t.last_user_message.trim() ? endMs : null);
      }
      // A Codex parent's children are tallied under the collected rollout
      // stem while this row may have been initialized from the bare eBPF
      // UUID (count 0). Carry the collected parent's count over, taking the
      // max so an exact-ID parent — already counted at initialization — is
      // not double-counted.
      existing.subagent_count = Math.max(
        existing.subagent_count,
        subagentCount.get(t.session_id) ?? 0,
      );
      if (lastMs !== null && (existing.last_active_ms === null || lastMs > existing.last_active_ms)) {
        existing.last_active_ms = lastMs;
      }
    } else {
      lastPreviewMsById.set(t.session_id, t.last_user_message?.trim() ? endMs : null);
      byId.set(t.session_id, {
        session_id: t.session_id,
        sources: ['log'],
        agent_name: t.agent_name || null,
        project: t.project || null,
        model: t.model_name,
        count: t.num_steps,
        input_tokens: t.total_prompt_tokens,
        output_tokens: t.total_completion_tokens,
        first_message: t.first_user_message ?? null,
        last_message: t.last_user_message ?? null,
        last_active_ms: lastMs,
        subagent_count: subagentCount.get(t.session_id) ?? 0,
      });
    }
  }

  return Array.from(byId.values()).sort(
    (a, b) => (b.last_active_ms ?? 0) - (a.last_active_ms ?? 0),
  );
}
