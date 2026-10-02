import React, { useState, useEffect, useCallback, useMemo } from 'react';
import { useNavigate } from 'react-router-dom';
import {
  fetchSessions,
  fetchTrajectories,
  fetchOptimizeConfig,
  semanticSearchSessions,
} from '../utils/apiClient';
import type {
  SessionSummary,
  TrajectorySummary,
  SemanticSearchResult,
} from '../utils/apiClient';
import { applySemanticRanking } from '../utils/semanticSearchFilter';
import { CopyButton } from '../components/CopyButton';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';

// ─── Merged session model ─────────────────────────────────────────────────────

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

/** Last activity of a log-collected trajectory in epoch milliseconds. */
function trajectoryLastActiveMs(t: TrajectorySummary): number | null {
  if (t.end_time) {
    const ms = Date.parse(t.end_time);
    if (!Number.isNaN(ms)) return ms;
  }
  return t.collected_at_ns > 0 ? Math.floor(t.collected_at_ns / 1_000_000) : null;
}

/** Trailing 36-char UUID of a Codex rollout stem (`rollout-<ts>-<uuid>`). */
const TRAILING_UUID_RE =
  /([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})$/;

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
  const byId = new Map<string, MergedSession>();

  // Count subagents per parent session. A subagent's session_id follows the
  // composite form "<parent>:subagent:<child>"; tally by the parent prefix.
  const subagentCount = new Map<string, number>();
  for (const t of logRows) {
    if (!t.is_subagent) continue;
    const idx = t.session_id.indexOf(':subagent:');
    if (idx > 0) {
      const parent = t.session_id.slice(0, idx);
      subagentCount.set(parent, (subagentCount.get(parent) ?? 0) + 1);
    }
  }

  for (const s of ebpfRows) {
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
      last_active_ms: s.last_seen_ns > 0 ? Math.floor(s.last_seen_ns / 1_000_000) : null,
      subagent_count: subagentCount.get(s.session_id) ?? 0,
    });
  }

  for (const t of logRows) {
    // Subagent trajectories belong to their parent task — don't show them as
    // independent rows. They remain reachable via the parent's ATIF viewer
    // (breadcrumb navigation). Only keep orphaned subagents whose parent is
    // absent from both sources (e.g. parent outside the selected time range).
    if (t.is_subagent) {
      const idx = t.session_id.indexOf(':subagent:');
      const parentId = idx > 0 ? t.session_id.slice(0, idx) : null;
      if (parentId && (byId.has(parentId) || logRows.some((l) => l.session_id === parentId))) {
        continue;
      }
    }
    const lastMs = trajectoryLastActiveMs(t);
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
      // The log side carries the full trajectory — its previews win.
      existing.first_message = t.first_user_message ?? existing.first_message;
      existing.last_message = t.last_user_message ?? existing.last_message;
      if (lastMs !== null && (existing.last_active_ms === null || lastMs > existing.last_active_ms)) {
        existing.last_active_ms = lastMs;
      }
    } else {
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

// ─── Helpers ──────────────────────────────────────────────────────────────────

export function timeAgo(
  t: (key: MessageKey, params?: Record<string, string | number>) => string,
  localeTag: string,
  ms: number | null,
): string {
  if (ms === null) return '-';
  const diff = Math.floor((Date.now() - ms) / 1000);
  if (diff < 60) return t('common.justNow');
  if (diff < 3600) return t('common.minutesAgo', { n: Math.floor(diff / 60) });
  if (diff < 86400) return t('common.hoursAgo', { n: Math.floor(diff / 3600) });
  if (diff < 604800) return t('as.daysAgo', { n: Math.floor(diff / 86400) });
  return new Date(ms).toLocaleDateString(localeTag);
}

function fmtTokens(n: number | null): string {
  if (n === null) return '-';
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`;
  return String(n);
}

const PAGE_SIZE = 15;

const TIME_PRESETS: { labelKey: MessageKey; ms: number }[] = [
  { labelKey: 'as.timePreset.last24h', ms: 24 * 3600 * 1000 },
  { labelKey: 'as.timePreset.last7d', ms: 7 * 24 * 3600 * 1000 },
  { labelKey: 'as.timePreset.last30d', ms: 30 * 24 * 3600 * 1000 },
];

const SOURCE_LABEL: Record<SessionSource, MessageKey> = { ebpf: 'as.source.ebpf', log: 'as.source.log' };

const SourceBadge: React.FC<{ sources?: SessionSource[] }> = ({ sources }) => {
  const { t } = useI18n();
  const sourceRows = Array.isArray(sources) ? sources : [];
  return (
    <span className="inline-flex gap-1">
      {sourceRows.map((s) => (
        <span
          key={s}
          className={`px-1.5 py-0.5 rounded text-xs font-medium ${
            s === 'ebpf' ? 'bg-blue-100 text-blue-700' : 'bg-emerald-100 text-emerald-700'
          }`}
        >
          {t(SOURCE_LABEL[s])}
        </span>
      ))}
    </span>
  );
};

// ─── Main Page ────────────────────────────────────────────────────────────────

export const AgentSessionsPage: React.FC = () => {
  const { t } = useI18n();
  const localeTag = useLocaleTag();
  const navigate = useNavigate();
  const [merged, setMerged] = useState<MergedSession[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [rangeMs, setRangeMs] = useState(7 * 24 * 3600 * 1000);
  const [sourceFilter, setSourceFilter] = useState<'all' | SessionSource>('all');
  const [agentFilter, setAgentFilter] = useState('all');
  const [search, setSearch] = useState('');
  const [semanticEnabled, setSemanticEnabled] = useState(false);
  const [semanticLoading, setSemanticLoading] = useState(false);
  const [semanticMatches, setSemanticMatches] = useState<Record<string, SemanticSearchResult>>({});
  const [page, setPage] = useState(1);
  const [autoRefresh, setAutoRefresh] = useState(false);

  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const endNs = Date.now() * 1_000_000;
      const startNs = endNs - rangeMs * 1_000_000;
      // /api/trajectories degrades to [] when collection never ran.
      const [ebpf, logs] = await Promise.all([
        fetchSessions(startNs, endNs),
        fetchTrajectories().catch(() => [] as TrajectorySummary[]),
      ]);
      setMerged(mergeSessions(ebpf, logs));
      setError(null);
    } catch (e: any) {
      setError(e.message || t('as.fetchError'));
    } finally {
      setLoading(false);
    }
  }, [rangeMs, t]);

  useEffect(() => {
    loadData();
  }, [loadData]);

  // Auto-refresh every 10s when enabled
  useEffect(() => {
    if (!autoRefresh) return;
    const interval = setInterval(loadData, 10_000);
    return () => clearInterval(interval);
  }, [autoRefresh, loadData]);

  // Semantic search is an optional enhancement; it only runs when the LLM is
  // configured in the optimization settings.
  useEffect(() => {
    fetchOptimizeConfig()
      .then((cfg) => setSemanticEnabled(Boolean(cfg.configured)))
      .catch(() => setSemanticEnabled(false));
  }, []);

  // Clear stale semantic results as soon as the query or filters change, so
  // the 500ms debounce window never shows results for the previous query.
  // Enter the loading state immediately too: the LLM request does not start
  // until the debounce fires, and without this flag the table would flash
  // "no matching sessions" during that window.
  useEffect(() => {
    setSemanticMatches({});
    setSemanticLoading(search.trim().length > 0 && semanticEnabled);
  }, [search, sourceFilter, agentFilter, rangeMs, semanticEnabled]);

  // Candidate pool shared by the debounce effect and the `filtered` memo:
  // sessions within the selected time range, source, and agent filters.
  // /api/trajectories has no time-range support, so log-collected sessions
  // must be pruned client-side to honour the selected range; otherwise the
  // list appears unchanged when switching presets.
  const base = useMemo(() => {
    const rangeStartMs = Date.now() - rangeMs;
    return merged.filter((s) => {
      if (s.last_active_ms !== null && s.last_active_ms < rangeStartMs) return false;
      const sources = Array.isArray(s.sources) ? s.sources : [];
      if (sourceFilter !== 'all' && !sources.includes(sourceFilter)) return false;
      if (
        agentFilter !== 'all' &&
        (s.agent_name ?? '').toLowerCase() !== agentFilter.toLowerCase()
      ) {
        return false;
      }
      return true;
    });
  }, [merged, rangeMs, sourceFilter, agentFilter]);

  // Debounce the LLM semantic search. Pure-LLM search: no client-side
  // substring fast path — every query goes through the backend ranking.
  useEffect(() => {
    const query = search.trim();
    if (!query || !semanticEnabled) {
      setSemanticMatches({});
      setSemanticLoading(false);
      return;
    }

    let cancelled = false;
    const timer = setTimeout(() => {
      if (base.length <= 5) {
        setSemanticMatches({});
        setSemanticLoading(false);
        return;
      }

      const candidates = base.slice(0, 50).map((s) => ({
        session_id: s.session_id,
        first_message: s.first_message,
        last_message: s.last_message,
        project: s.project,
      }));

      setSemanticLoading(true);
      semanticSearchSessions({ query, candidates })
        .then((res) => {
          if (cancelled) return;
          const next: Record<string, SemanticSearchResult> = {};
          for (const r of res.results) {
            next[r.session_id] = {
              ...r,
              relevance: r.relevance === 'high' ? 'high' : 'medium',
            };
          }
          setSemanticMatches(next);
        })
        .catch(() => {
          if (!cancelled) setSemanticMatches({});
        })
        .finally(() => {
          if (!cancelled) setSemanticLoading(false);
        });
    }, 500);

    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [search, semanticEnabled, base]);

  // Reset to page 1 when filters change
  useEffect(() => {
    setPage(1);
  }, [sourceFilter, agentFilter, search, rangeMs]);

  // Agent filter options, deduplicated case-insensitively: the eBPF side
  // reports "Qoder" while the log collector writes "qoder" — they are the
  // same agent. Keep the first-seen casing as the display label.
  const agentNames = useMemo(() => {
    const byLower = new Map<string, string>();
    merged.forEach((s) => {
      if (!s.agent_name) return;
      const key = s.agent_name.toLowerCase();
      if (!byLower.has(key)) byLower.set(key, s.agent_name);
    });
    return Array.from(byLower.values()).sort();
  }, [merged]);

  const filtered = useMemo(() => {
    return applySemanticRanking(base, search, semanticMatches, semanticLoading);
  }, [base, search, semanticMatches, semanticLoading]);

  const totalPages = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const safePage = Math.min(page, totalPages);
  const paged = filtered.slice((safePage - 1) * PAGE_SIZE, safePage * PAGE_SIZE);

  return (
    <div className="p-6 max-w-screen-xl mx-auto space-y-4">
      {/* ── Toolbar: total + time range + refresh ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-center gap-4">
        <span className="text-sm text-gray-600">
          {t('as.totalSessions', { n: filtered.length })}
        </span>
        <div className="flex gap-2">
          {TIME_PRESETS.map(({ labelKey, ms }) => (
            <button
              key={labelKey}
              onClick={() => setRangeMs(ms)}
              className={`px-3 py-1.5 text-xs rounded-lg transition-colors ${
                rangeMs === ms
                  ? 'bg-blue-100 text-blue-700 font-medium'
                  : 'bg-gray-100 hover:bg-gray-200 text-gray-600'
              }`}
            >
              {t(labelKey)}
            </button>
          ))}
        </div>
        <div className="ml-auto flex items-center gap-3">
          <label className="flex items-center gap-1.5 text-xs text-gray-600">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(e) => setAutoRefresh(e.target.checked)}
            />
            {t('as.autoRefresh')}
          </label>
          <button
            onClick={loadData}
            disabled={loading}
            className="px-4 py-1.5 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 disabled:opacity-50 transition-colors"
          >
            {loading ? t('common.loading') : t('common.refresh')}
          </button>
        </div>
      </div>

      {/* ── Filters: source tabs + agent + search ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-center gap-4">
        <div className="flex gap-1">
          {([
            { id: 'all', labelKey: 'as.filter.allSources' as MessageKey },
            { id: 'ebpf', labelKey: 'as.source.ebpf' as MessageKey },
            { id: 'log', labelKey: 'as.source.log' as MessageKey },
          ] as const).map(({ id, labelKey }) => (
            <button
              key={id}
              onClick={() => setSourceFilter(id)}
              className={`px-3 py-1.5 text-xs rounded-lg transition-colors ${
                sourceFilter === id
                  ? 'bg-blue-100 text-blue-700 font-medium'
                  : 'bg-gray-100 hover:bg-gray-200 text-gray-600'
              }`}
            >
              {t(labelKey)}
            </button>
          ))}
        </div>
        <select
          className="border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400 min-w-[140px]"
          value={agentFilter}
          onChange={(e) => setAgentFilter(e.target.value)}
        >
          <option value="all">{t('common.allAgents')}</option>
          {agentNames.map((a) => (
            <option key={a} value={a}>{a}</option>
          ))}
        </select>
        <input
          type="text"
          placeholder={t('as.searchPlaceholder')}
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="flex-1 min-w-[220px] border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400"
        />
        {semanticLoading && (
          <span className="text-xs text-blue-500 whitespace-nowrap">AI ...</span>
        )}
      </div>

      {error && (
        <div className="bg-red-50 border border-red-200 text-red-700 px-4 py-2 rounded text-sm">
          {error}
        </div>
      )}

      {/* ── Session table ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 overflow-hidden">
        {loading && merged.length === 0 ? (
          <div className="p-10 text-center text-gray-500 text-sm">{t('as.loadingSessions')}</div>
        ) : filtered.length === 0 ? (
          <div className="p-10 text-center text-gray-500 text-sm">
            {semanticLoading
              ? t('as.semanticSearching')
              : search
              ? semanticEnabled
                ? t('as.noMatchingSessions')
                : t('as.semanticSearchUnavailable')
              : sourceFilter !== 'all' || agentFilter !== 'all'
                ? t('as.noMatchingSessions')
                : t('as.noSessionsInRange')}
          </div>
        ) : (
          <table className="w-full text-sm">
            <thead>
              <tr className="bg-gray-50 text-left text-xs text-gray-500 uppercase">
                <th className="px-4 py-3">{t('as.source')}</th>
                <th className="px-4 py-3">{t('common.agent')}</th>
                <th className="px-4 py-3">{t('as.project')}</th>
                <th className="px-4 py-3">{t('as.firstMessage')}</th>
                <th className="px-4 py-3">{t('as.lastMessage')}</th>
                <th className="px-4 py-3 text-right">{t('as.lastActive')}</th>
                <th className="px-4 py-3 text-center">{t('common.actions')}</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {paged.map((s) => (
                <tr
                  key={s.session_id}
                  className="hover:bg-gray-50 cursor-pointer transition-colors"
                  onClick={() => window.open(`#/atif?type=session&id=${encodeURIComponent(s.session_id)}`, '_blank')}
                  title={t('as.openInNewWindow')}
                >
                  <td className="px-4 py-3"><SourceBadge sources={s.sources} /></td>
                  <td className="px-4 py-3">
                    <div className="text-gray-800">{s.agent_name ?? '-'}</div>
                  </td>
                  <td className="px-4 py-3">
                    <div className="text-gray-700 max-w-[150px] truncate" title={s.project ?? ''}>
                      {s.project ?? '-'}
                    </div>
                    <div className="flex items-center gap-1">
                      <span className="font-mono text-xs text-gray-400 max-w-[150px] truncate" title={s.session_id}>
                        {s.session_id}
                      </span>
                      <CopyButton text={s.session_id} title={t('as.copySessionId')} />
                    </div>
                    {semanticMatches[s.session_id] && (
                      <span
                        className="inline-flex items-center gap-1 mt-1 px-1.5 py-0.5 bg-blue-50 text-blue-600 rounded text-xs font-medium"
                        title={semanticMatches[s.session_id].reason}
                      >
                        AI · {semanticMatches[s.session_id].relevance}
                      </span>
                    )}
                    {s.subagent_count > 0 && (
                      <span
                        className="inline-flex items-center gap-1 mt-1 px-1.5 py-0.5 bg-indigo-50 text-indigo-600 rounded text-xs font-medium"
                        title={t('as.subagentBadgeTitle', { n: s.subagent_count })}
                      >
                        🤖 {t('as.subagentCount', { n: s.subagent_count })}
                      </span>
                    )}
                  </td>
                  <td className="px-4 py-3 text-gray-600">
                    <div className="max-w-[260px] truncate" title={s.first_message ?? ''}>
                      {s.first_message ?? <span className="text-gray-300">-</span>}
                    </div>
                  </td>
                  <td className="px-4 py-3 text-gray-600">
                    <div className="max-w-[260px] truncate" title={s.last_message ?? ''}>
                      {s.last_message ?? <span className="text-gray-300">-</span>}
                    </div>
                  </td>
                  <td
                    className="px-4 py-3 text-right text-gray-600 whitespace-nowrap"
                    title={t('as.metricsTooltip', { count: s.count, inTokens: fmtTokens(s.input_tokens), outTokens: fmtTokens(s.output_tokens) })}
                  >
                    <span title={s.last_active_ms ? new Date(s.last_active_ms).toLocaleString(localeTag) : ''}>
                      {timeAgo(t, localeTag, s.last_active_ms)}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-center" onClick={(e) => e.stopPropagation()}>
                    <button
                      onClick={() => navigate(`/optimization/${encodeURIComponent(s.session_id)}`)}
                      className="px-3 py-1 text-xs bg-blue-50 text-blue-700 border border-blue-200 rounded-lg hover:bg-blue-100 transition-colors"
                      title={t('as.runOptimizationTitle')}
                    >
                      {t('as.analyze')}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      {/* ── Pagination ── */}
      {filtered.length > PAGE_SIZE && (
        <div className="flex items-center justify-between text-sm text-gray-600">
          <span>
            {t('as.paginationSummary', { total: filtered.length, cur: safePage, totalPages })}
          </span>
          <div className="flex gap-1">
            <button
              disabled={safePage <= 1}
              onClick={() => setPage(1)}
              className="px-2.5 py-1 rounded border border-gray-300 disabled:opacity-40 hover:bg-gray-100"
            >
              «
            </button>
            <button
              disabled={safePage <= 1}
              onClick={() => setPage((p) => Math.max(1, p - 1))}
              className="px-2.5 py-1 rounded border border-gray-300 disabled:opacity-40 hover:bg-gray-100"
            >
              ‹
            </button>
            <button
              disabled={safePage >= totalPages}
              onClick={() => setPage((p) => Math.min(totalPages, p + 1))}
              className="px-2.5 py-1 rounded border border-gray-300 disabled:opacity-40 hover:bg-gray-100"
            >
              ›
            </button>
            <button
              disabled={safePage >= totalPages}
              onClick={() => setPage(totalPages)}
              className="px-2.5 py-1 rounded border border-gray-300 disabled:opacity-40 hover:bg-gray-100"
            >
              »
            </button>
          </div>
        </div>
      )}
    </div>
  );
};
