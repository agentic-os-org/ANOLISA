import React, { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { useNavigate, useSearchParams } from 'react-router-dom';
import {
  LineChart, Line, BarChart, Bar,
  XAxis, YAxis, CartesianGrid, Tooltip, Legend, ResponsiveContainer,
} from 'recharts';
import { InterruptionBadge } from '../components/InterruptionBadge';
import { InterruptionPanel, ResolvedEventInfo } from '../components/InterruptionPanel';
import { EvaluationBadge } from '../components/EvaluationBadge';
import { EvaluationPanel } from '../components/EvaluationPanel';
import { DateTimePicker } from '../components/DateTimePicker';
import { SessionIdHelp } from '../components/SessionIdHelp';
import { SessionResourceChart } from '../components/SessionResourceChart';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';
import { formatNsPadded as nsToDate } from '../utils/datetime';
import {
  fetchSessions,
  fetchTraces,
  fetchAgentNames,
  fetchTimeseries,
  fetchInterruptionCount,
  fetchInterruptionStats,
  fetchInterruptionSessionCounts,
  fetchInterruptionConversationCounts,
  fetchLatestEvaluation,
  fetchTokenSavings,
  conversationInterruptionKey,
  UNASSIGNED_INTERRUPTION_BUCKET,
  SessionSummary,
  TraceSummary,
  TimeseriesBucket,
  ModelTimeseriesBucket,
  InterruptionCountResponse,
  InterruptionTypeStat,
  SessionInterruptionCount,
  ConversationInterruptionCount,
  EvaluationResult,
} from '../utils/apiClient';

// ─── Helpers ──────────────────────────────────────────────────────────────────

/** Truncate a long ID for display */
function shortId(id: string, len = 16): string {
  return id.length > len ? id.slice(0, len) + '…' : id;
}

/** Copy button with a brief "Copied" feedback */
const CopyButton: React.FC<{ text: string }> = ({ text }) => {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    const done = () => {
      setCopied(true);
      if (timerRef.current) clearTimeout(timerRef.current);
      timerRef.current = setTimeout(() => setCopied(false), 1500);
    };
    // Clipboard API may be unavailable over HTTP; fall back to execCommand
    if (navigator.clipboard && window.isSecureContext) {
      navigator.clipboard.writeText(text).then(done).catch(() => fallbackCopy(text, done));
    } else {
      fallbackCopy(text, done);
    }
  };
  return (
    <button
      onClick={handleCopy}
      className={`flex-shrink-0 px-1.5 py-0.5 rounded text-xs transition-colors ${
        copied
          ? 'bg-green-100 text-green-600'
          : 'bg-gray-100 hover:bg-gray-200 text-gray-500 hover:text-gray-700'
      }`}
      title={t('common.copyFullId')}
    >
      {copied ? t('common.copied') : t('common.copy')}
    </button>
  );
};

function fallbackCopy(text: string, done: () => void) {
  const el = document.createElement('textarea');
  el.value = text;
  el.style.position = 'fixed';
  el.style.opacity = '0';
  document.body.appendChild(el);
  el.focus();
  el.select();
  try { document.execCommand('copy'); } catch {}
  document.body.removeChild(el);
  done();
}

/** Format token number */
function fmtTokens(n: number): string {
  return n.toLocaleString();
}

// ─── Trace sub-table ──────────────────────────────────────────────────────────

interface TraceSubTableProps {
  sessionId: string;
  conversationInterruptionCounts: Map<string, ConversationInterruptionCount>;
  startNs?: number;
  endNs?: number;
  onResolvedEvent?: (info: ResolvedEventInfo) => void;
}

const PAGE_SIZE = 10;

const TraceSubTable: React.FC<TraceSubTableProps> = ({ sessionId, conversationInterruptionCounts, startNs, endNs, onResolvedEvent }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const navigate = useNavigate();
  const [traces, setTraces] = useState<TraceSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [page, setPage] = useState(0); // 0-based
  const [expandedTracePanel, setExpandedTracePanel] = useState<string | null>(null);
  const [expandedEvaluationPanel, setExpandedEvaluationPanel] = useState<string | null>(null);
  const [evaluations, setEvaluations] = useState<Map<string, EvaluationResult>>(new Map());
  const [evaluationLookupDone, setEvaluationLookupDone] = useState<Set<string>>(new Set());
  const [evaluationLookupFailed, setEvaluationLookupFailed] = useState<Set<string>>(new Set());

  useEffect(() => {
    setLoading(true);
    setPage(0);
    setEvaluations(new Map());
    setEvaluationLookupDone(new Set());
    setEvaluationLookupFailed(new Set());
    fetchTraces(sessionId, startNs, endNs)
      .then(setTraces)
      .catch((e: Error) => setError(e.message))
      .finally(() => setLoading(false));
  }, [sessionId, startNs, endNs]);

  const totalPages = Math.max(1, Math.ceil(traces.length / PAGE_SIZE));
  const pageTraces = useMemo(
    () => traces.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE),
    [page, traces]
  );

  useEffect(() => {
    if (loading || pageTraces.length === 0) return;

    const missing = pageTraces.filter(
      (trace) =>
        !evaluationLookupDone.has(trace.conversation_id) &&
        !evaluationLookupFailed.has(trace.conversation_id)
    );
    if (missing.length === 0) return;

    let cancelled = false;
    Promise.all(
      missing.map((trace) =>
        fetchLatestEvaluation(trace.conversation_id)
          .then((result) => ({
            conversationId: trace.conversation_id,
            result,
            ok: true,
          }))
          .catch(() => ({
            conversationId: trace.conversation_id,
            result: null,
            ok: false,
          }))
      )
    ).then((entries) => {
      if (cancelled) return;
      setEvaluations((prev) => {
        const next = new Map(prev);
        for (const entry of entries) {
          if (entry.ok && entry.result) {
            next.set(entry.conversationId, entry.result);
          }
        }
        return next;
      });
      setEvaluationLookupDone((prev) => {
        const next = new Set(prev);
        let changed = false;
        for (const entry of entries) {
          if (entry.ok && !next.has(entry.conversationId)) {
            next.add(entry.conversationId);
            changed = true;
          }
        }
        return changed ? next : prev;
      });
      setEvaluationLookupFailed((prev) => {
        const next = new Set(prev);
        let changed = false;
        for (const entry of entries) {
          if (entry.ok) {
            if (next.delete(entry.conversationId)) changed = true;
          } else if (!next.has(entry.conversationId)) {
            next.add(entry.conversationId);
            changed = true;
          }
        }
        return changed ? next : prev;
      });
    });

    return () => {
      cancelled = true;
    };
  }, [evaluationLookupDone, evaluationLookupFailed, loading, pageTraces]);

  if (loading)
    return (
      <tr>
        <td colSpan={10} className="px-8 py-4 text-sm text-gray-400 bg-blue-50">
          {t('cl.loadingTraces')}
        </td>
      </tr>
    );
  if (error)
    return (
      <tr>
        <td colSpan={10} className="px-8 py-4 text-sm text-red-500 bg-blue-50">
          ⚠️ {error}
        </td>
      </tr>
    );

  return (
    <>
      <tr className="bg-blue-50 border-t border-blue-100">
        <td colSpan={10} className="px-4 lg:px-8">
          <SessionResourceChart sessionId={sessionId} startNs={startNs} endNs={endNs} />
        </td>
      </tr>
      {/* Sub-header */}
      <tr className="bg-blue-50 border-t border-blue-100">
        <td colSpan={10} className="px-4 lg:px-8 py-2">
          <div className="grid grid-cols-[230px_244px_110px_110px_150px_74px_100px_90px] text-xs font-semibold text-blue-700 uppercase tracking-wide min-w-[900px]">
            <div>{t('cl.conversationId')}</div>
            <div>{t('cl.userQuery')}</div>
            <div>{t('cl.inputTokens')}</div>
            <div>{t('cl.outputTokens')}</div>
            <div>{t('cl.startTime')}</div>
            <div>{t('cl.actions')}</div>
            <div>{t('cl.qualityEval')}</div>
            <div>{t('cl.interrupts')}</div>
          </div>
        </td>
      </tr>

      {traces.length === 0 && (
        <tr className="bg-blue-50">
          <td colSpan={10} className="px-4 lg:px-8 py-3 text-sm text-gray-400">
            {t('cl.noTraces')}
          </td>
        </tr>
      )}

      {/* Interruptions this session owns but that never got a conversation_id.
          Without this row they would count toward the session badge and then
          vanish on expand, since the rows below only look up counts by a real
          trace's conversation_id. */}
      {(() => {
        const ic = conversationInterruptionCounts.get(
          conversationInterruptionKey(sessionId, UNASSIGNED_INTERRUPTION_BUCKET)
        );
        if (!ic || ic.total === 0) return null;
        return (
          <tr className="bg-amber-50 border-t border-amber-100">
            <td colSpan={10} className="px-4 lg:px-8 py-3">
              <div className="grid grid-cols-[230px_244px_110px_110px_150px_74px_100px_90px] items-center text-xs min-w-[900px]">
                <div className="text-amber-800">{t('cl.unassignedConversation')}</div>
                <div className="col-span-6 text-amber-700">
                  {t('cl.unassignedConversationHint')}
                </div>
                <div>
                  <InterruptionBadge bySeverity={ic.by_severity} types={ic.types} />
                </div>
              </div>
            </td>
          </tr>
        );
      })()}

      {pageTraces.map((tr) => (
        <React.Fragment key={tr.conversation_id}>
          <tr className="bg-blue-50 hover:bg-blue-100 transition-colors">
            <td colSpan={10} className="px-4 lg:px-8 py-2">
              <div className="grid grid-cols-[230px_244px_110px_110px_150px_74px_100px_90px] items-center text-sm min-w-[900px]">
                {/* Col 1: Conversation ID */}
                <div className="min-w-0 pr-2">
                  <div className="flex items-center gap-1">
                    <span
                      className="font-mono text-xs text-blue-600 block truncate"
                      title={tr.conversation_id}
                    >
                      {shortId(tr.conversation_id, 18)}
                    </span>
                    <CopyButton text={tr.conversation_id} />
                  </div>
                </div>
                {/* Col 2: User query */}
                <div className="min-w-0 pr-2">
                  {tr.user_query ? (
                    <div
                      className="text-sm text-gray-800 truncate"
                      title={tr.user_query}
                    >
                      {tr.user_query}
                    </div>
                  ) : (
                    <span className="text-xs text-gray-400">—</span>
                  )}
                </div>
                <div className="text-blue-600 font-semibold">
                  {fmtTokens(tr.total_input_tokens)}
                </div>
                <div className="text-green-600 font-semibold">
                  {fmtTokens(tr.total_output_tokens)}
                </div>
                <div className="text-xs text-gray-500">{nsToDate(tr.start_ns, locale)}</div>
                <div>
                  <button
                    onClick={() => navigate(`/atif?type=conversation&id=${encodeURIComponent(tr.conversation_id)}`)}
                    className="px-3 py-1 bg-white border border-blue-300 text-blue-700 rounded-lg text-xs hover:bg-blue-50 transition-colors"
                  >
                    {t('common.details')}
                  </button>
                </div>
                <div>
                  {(() => {
                    const evaluation = evaluations.get(tr.conversation_id);
                    const lookupFailed = evaluationLookupFailed.has(tr.conversation_id);
                    return (
                      <button
                        onClick={() => {
                          if (lookupFailed) {
                            setEvaluationLookupFailed((prev) => {
                              if (!prev.has(tr.conversation_id)) return prev;
                              const next = new Set(prev);
                              next.delete(tr.conversation_id);
                              return next;
                            });
                            return;
                          }
                          setExpandedEvaluationPanel(
                            expandedEvaluationPanel === tr.conversation_id ? null : tr.conversation_id
                          );
                        }}
                        className="inline-flex min-w-[72px] items-center justify-center rounded border border-gray-200 bg-white px-2 py-1 text-xs text-gray-600 hover:bg-gray-50"
                      >
                        {evaluation ? (
                          <EvaluationBadge result={evaluation} />
                        ) : lookupFailed ? (
                          <span
                            className="text-red-600"
                            title={t('cl.loadFailedTooltip')}
                          >
                            {t('cl.loadFailed')}
                          </span>
                        ) : (
                          t('cl.eval')
                        )}
                      </button>
                    );
                  })()}
                </div>
                <div>
                  {(() => {
                    const ic = conversationInterruptionCounts.get(
                      conversationInterruptionKey(sessionId, tr.conversation_id)
                    );
                    if (!ic || ic.total === 0) return <span className="text-xs text-gray-300">—</span>;
                    return (
                      <InterruptionBadge
                        bySeverity={ic.by_severity}
                        types={ic.types}
                        onClick={() => setExpandedTracePanel(
                          expandedTracePanel === tr.conversation_id ? null : tr.conversation_id
                        )}
                      />
                    );
                  })()}
                </div>
              </div>
              </td>
            </tr>
          {/* Trace evaluation panel */}
          {expandedEvaluationPanel === tr.conversation_id && (
            <tr className="bg-blue-50">
              <td colSpan={10} className="px-4 lg:px-8 pb-3 pt-0">
                <EvaluationPanel
                  conversationId={tr.conversation_id}
                  initialResult={evaluations.get(tr.conversation_id) ?? null}
                  onResult={(result) => setEvaluations((prev) => {
                    const next = new Map(prev);
                    next.set(tr.conversation_id, result);
                    return next;
                  })}
                />
              </td>
            </tr>
          )}
          {/* Trace interruption panel */}
          {expandedTracePanel === tr.conversation_id && (
            <tr className="bg-blue-50">
              <td colSpan={10} className="px-4 lg:px-8 pb-3 pt-0">
                <div className="border border-gray-200 rounded-lg overflow-hidden">
                  <InterruptionPanel
                    sessionId={sessionId}
                    conversationId={tr.conversation_id}
                    onClose={() => setExpandedTracePanel(null)}
                    onResolvedEvent={onResolvedEvent}
                  />
                </div>
              </td>
            </tr>
          )}
        </React.Fragment>
      ))}

      {/* Pagination controls */}
      {totalPages > 1 && (
        <tr className="bg-blue-50 border-t border-blue-100">
          <td colSpan={10} className="px-4 lg:px-8 py-2">
            <div className="flex items-center gap-2 justify-end">
              <span className="text-xs text-gray-500">
                {page * PAGE_SIZE + 1}–{Math.min((page + 1) * PAGE_SIZE, traces.length)} / {traces.length}
              </span>
              <button
                onClick={() => setPage((p) => Math.max(0, p - 1))}
                disabled={page === 0}
                className="px-2 py-0.5 text-xs border border-blue-300 text-blue-700 rounded hover:bg-blue-50 disabled:opacity-40 transition-colors"
              >
                &lsaquo; {t('common.prev')}
              </button>
              {Array.from({ length: totalPages }, (_, i) => (
                <button
                  key={i}
                  onClick={() => setPage(i)}
                  className={`px-2 py-0.5 text-xs rounded transition-colors ${
                    i === page
                      ? 'bg-blue-600 text-white'
                      : 'border border-blue-300 text-blue-700 hover:bg-blue-50'
                  }`}
                >
                  {i + 1}
                </button>
              ))}
              <button
                onClick={() => setPage((p) => Math.min(totalPages - 1, p + 1))}
                disabled={page === totalPages - 1}
                className="px-2 py-0.5 text-xs border border-blue-300 text-blue-700 rounded hover:bg-blue-50 disabled:opacity-40 transition-colors"
              >
                {t('common.next')} &rsaquo;
              </button>
            </div>
          </td>
        </tr>
      )}
    </>
  );
};

// ─── Time-series chart helpers ────────────────────────────────────────────────

/** Palette for model colors */
const MODEL_COLORS = [
  '#6366f1', '#10b981', '#f59e0b', '#ef4444', '#3b82f6',
  '#ec4899', '#14b8a6', '#8b5cf6', '#f97316', '#06b6d4',
];

/** Axis label: HH:MM for intra-day, MM-DD HH:MM for multi-day spans */
function nsToLabel(ns: number, spanMs: number): string {
  const d = new Date(ns / 1_000_000);
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  if (spanMs > 23 * 3600 * 1000) {
    return `${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')} ${hm}`;
  }
  return hm;
}

/**
 * Fill sparse bucket array to a full dense series.
 * Backend only returns buckets that have events; missing ones become 0-value entries.
 */
function fillTokenBuckets(
  data: TimeseriesBucket[],
  startNs: number,
  endNs: number,
  bucketCount: number,
): TimeseriesBucket[] {
  const bucketNs = Math.floor((endNs - startNs) / Math.max(bucketCount, 1));
  if (bucketNs <= 0) return data;
  const byIdx = new Map<number, TimeseriesBucket>();
  for (const b of data) {
    const idx = Math.floor((b.bucket_start_ns - startNs) / bucketNs);
    byIdx.set(idx, b);
  }
  const result: TimeseriesBucket[] = [];
  for (let i = 0; i < bucketCount; i++) {
    result.push(byIdx.get(i) ?? {
      bucket_start_ns: startNs + i * bucketNs,
      input_tokens: 0,
      output_tokens: 0,
      total_tokens: 0,
    });
  }
  return result;
}

function fillModelBuckets(
  data: ModelTimeseriesBucket[],
  startNs: number,
  endNs: number,
  bucketCount: number,
  models: string[],
): ModelTimeseriesBucket[] {
  const bucketNs = Math.floor((endNs - startNs) / Math.max(bucketCount, 1));
  if (bucketNs <= 0) return data;
  const byIdxModel = new Map<number, Map<string, number>>();
  for (const b of data) {
    const idx = Math.floor((b.bucket_start_ns - startNs) / bucketNs);
    if (!byIdxModel.has(idx)) byIdxModel.set(idx, new Map());
    byIdxModel.get(idx)!.set(b.model, b.total_tokens);
  }
  const result: ModelTimeseriesBucket[] = [];
  for (let i = 0; i < bucketCount; i++) {
    const bucketStartNs = startNs + i * bucketNs;
    const modelMap = byIdxModel.get(i);
    for (const model of models) {
      result.push({ bucket_start_ns: bucketStartNs, model, total_tokens: modelMap?.get(model) ?? 0 });
    }
  }
  return result;
}

// ─── Token Time-series Chart ──────────────────────────────────────────────────

interface TokenChartData {
  label: string;
  input: number;
  output: number;
  total: number;
}

interface TokenTimeseriesChartProps {
  data: TimeseriesBucket[];
  startNs: number;
  endNs: number;
  bucketCount?: number;
}

const TOKEN_SERIES = [
  { key: 'input', nameKey: 'cl.inputTokens' as const, color: '#3b82f6' },
  { key: 'output', nameKey: 'cl.outputTokens' as const, color: '#10b981' },
  { key: 'total', nameKey: 'cl.totalTokens' as const, color: '#6366f1' },
] as const;

const TokenTimeseriesChart: React.FC<TokenTimeseriesChartProps> = ({
  data, startNs, endNs, bucketCount = 30,
}) => {
  const { t } = useI18n();
  const spanMs = (endNs - startNs) / 1_000_000;
  const filled = fillTokenBuckets(data, startNs, endNs, bucketCount);
  const chartData: TokenChartData[] = filled.map((b) => ({
    label: nsToLabel(b.bucket_start_ns, spanMs),
    input: b.input_tokens,
    output: b.output_tokens,
    total: b.total_tokens,
  }));

  // Track which series are hidden; click legend item to toggle
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const toggleSeries = (key: string) => {
    setHidden((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });
  };

  if (filled.every((b) => b.total_tokens === 0)) {
    return (
      <div className="flex items-center justify-center h-32 text-gray-400 text-sm">
        {t('cl.noTimeseriesData')}
      </div>
    );
  }

  const tickStep = Math.max(1, Math.floor(bucketCount / 6));
  const ticks = chartData.filter((_, i) => i % tickStep === 0).map((d) => d.label);

  return (
    <ResponsiveContainer width="100%" height={220}>
      <LineChart data={chartData} margin={{ top: 4, right: 16, left: 0, bottom: 4 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="#f0f0f0" />
        <XAxis dataKey="label" ticks={ticks} tick={{ fontSize: 10 }} />
        <YAxis tick={{ fontSize: 11 }} width={56} />
        <Tooltip formatter={(v: number) => v.toLocaleString()} />
        <Legend
          wrapperStyle={{ fontSize: 12, cursor: 'pointer' }}
          onClick={(e) => toggleSeries(e.dataKey as string)}
          formatter={(value, entry) => (
            <span style={{ color: hidden.has((entry as any).dataKey) ? '#aaa' : (entry as any).color }}>
              {value}
            </span>
          )}
        />
        {TOKEN_SERIES.map(({ key, nameKey, color }) => (
          <Line
            key={key}
            type="monotone"
            dataKey={key}
            name={t(nameKey)}
            stroke={color}
            dot={false}
            strokeWidth={2}
            hide={hidden.has(key)}
          />
        ))}
      </LineChart>
    </ResponsiveContainer>
  );
};

// ─── Model Token Time-series Chart ────────────────────────────────────────────

interface ModelTimeseriesChartProps {
  data: ModelTimeseriesBucket[];
  startNs: number;
  endNs: number;
  bucketCount?: number;
}

const ModelTimeseriesChart: React.FC<ModelTimeseriesChartProps> = ({
  data, startNs, endNs, bucketCount = 30,
}) => {
  const { t } = useI18n();
  const spanMs = (endNs - startNs) / 1_000_000;
  const models = Array.from(new Set(data.map((d) => d.model))).sort();
  const filled = fillModelBuckets(data, startNs, endNs, bucketCount, models);

  const bucketMap = new Map<number, Record<string, number>>();
  for (const d of filled) {
    if (!bucketMap.has(d.bucket_start_ns)) bucketMap.set(d.bucket_start_ns, {});
    bucketMap.get(d.bucket_start_ns)![d.model] = d.total_tokens;
  }
  const chartData = Array.from(bucketMap.entries())
    .sort(([a], [b]) => a - b)
    .map(([ns, tokens]) => ({ label: nsToLabel(ns, spanMs), ...tokens }));

  // Track which model bars are hidden
  const [hidden, setHidden] = useState<Set<string>>(new Set());
  const toggleModel = (key: string) => {
    setHidden((prev) => {
      const next = new Set(prev);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });
  };

  if (models.length === 0) {
    return (
      <div className="flex items-center justify-center h-32 text-gray-400 text-sm">
        {t('cl.noModelTimeseriesData')}
      </div>
    );
  }

  const tickStep = Math.max(1, Math.floor(bucketCount / 6));
  const ticks = chartData.filter((_, i) => i % tickStep === 0).map((d) => d.label);

  return (
    <ResponsiveContainer width="100%" height={220}>
      <BarChart data={chartData} margin={{ top: 4, right: 16, left: 0, bottom: 4 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="#f0f0f0" />
        <XAxis dataKey="label" ticks={ticks} tick={{ fontSize: 10 }} />
        <YAxis tick={{ fontSize: 11 }} width={56} />
        <Tooltip formatter={(v: number) => v.toLocaleString()} />
        <Legend
          wrapperStyle={{ fontSize: 12, cursor: 'pointer' }}
          onClick={(e) => toggleModel(e.dataKey as string)}
          formatter={(value, entry) => {
            const color = hidden.has((entry as any).dataKey) ? '#aaa' : (entry as any).color;
            return <span style={{ color }}>{value}</span>;
          }}
        />
        {models.map((m, i) => (
          <Bar
            key={m}
            dataKey={m}
            name={m}
            stackId="model"
            fill={hidden.has(m) ? 'transparent' : MODEL_COLORS[i % MODEL_COLORS.length]}
          />
        ))}
      </BarChart>
    </ResponsiveContainer>
  );
};

// ─── Main Component ───────────────────────────────────────────────────────────

// Accept (and ignore) optional legacy props so App.tsx can still compile
// while we transition data ownership into this component.
export interface ConversationListProps {
  conversations?: unknown[];
}

export const ConversationList: React.FC<ConversationListProps> = () => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [searchParams, setSearchParams] = useSearchParams();

  // Restore state from URL params (set when navigating to detail page)
  const now = Date.now();
  const initStart = Number(searchParams.get('start')) || (now - 24 * 3600 * 1000);
  const initEnd   = Number(searchParams.get('end'))   || now;
  const initAgent = searchParams.get('agent') ?? '';

  // Time range state
  const [startMs, setStartMs] = useState(initStart);
  const [endMs, setEndMs] = useState(initEnd);

  // Agent name filter
  const [agentNames, setAgentNames] = useState<string[]>([]);
  const [selectedAgent, setSelectedAgent] = useState<string>(initAgent);
  const [agentNamesLoading, setAgentNamesLoading] = useState(false);

  // Sessions from backend
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Timeseries data
  const [tokenSeries, setTokenSeries] = useState<TimeseriesBucket[]>([]);
  const [modelSeries, setModelSeries] = useState<ModelTimeseriesBucket[]>([]);
  const [timeseriesLoading, setTimeseriesLoading] = useState(false);
  // The ns range actually used in the last query (for gap-filling in charts)
  const [queryRangeNs, setQueryRangeNs] = useState<[number, number]>([0, 1]);

  // Whether user has ever queried (controls showing charts/table)
  const [hasQueried, setHasQueried] = useState(false);

  // Interruption count for the queried time range
  const [interruptionCount, setInterruptionCount] = useState<InterruptionCountResponse | null>(null);
  // Per-type stats for tooltip breakdown
  const [interruptionStats, setInterruptionStats] = useState<InterruptionTypeStat[]>([]);

  // Interruption counts per session / conversation
  const [sessionInterruptionCounts, setSessionInterruptionCounts] = useState<Map<string, SessionInterruptionCount>>(new Map());
  const [conversationInterruptionCounts, setConversationInterruptionCounts] = useState<Map<string, ConversationInterruptionCount>>(new Map());

  // Token savings per session (session_id → compounded_saved with saved_tokens fallback)
  const [savingsMap, setSavingsMap] = useState<Map<string, number>>(new Map());

  // Which session row is expanded to show traces
  const [expandedSession, setExpandedSession] = useState<string | null>(null);

  // Session list pagination
  const SESSION_PAGE_SIZE = 10;
  const [sessionPage, setSessionPage] = useState(0);

  // Called when a single interruption event is resolved inside InterruptionPanel.
  // Decrements counts in all relevant state maps so badges update without re-query.
  const handleResolvedEvent = useCallback((info: ResolvedEventInfo) => {
    const sev = info.severity as 'critical' | 'high' | 'medium' | 'low';

    // 1. Update overview card total + by_severity
    setInterruptionCount(prev => {
      if (!prev) return prev;
      const newTotal = Math.max(0, prev.total - 1);
      return {
        total: newTotal,
        by_severity: { ...prev.by_severity, [sev]: Math.max(0, prev.by_severity[sev] - 1) },
      };
    });

    // 2. Update per-type stats (for tooltip)
    setInterruptionStats(prev =>
      prev.map(s =>
        s.severity === info.severity && s.interruption_type === info.interruption_type
          ? { ...s, count: Math.max(0, s.count - 1) }
          : s
      ).filter(s => s.count > 0)
    );

    // 3. Update conversation-level badge counts. A null id belongs to the
    //    unassigned bucket, which is a real row in the breakdown.
    {
      const convKey = conversationInterruptionKey(
        info.session_id ?? UNASSIGNED_INTERRUPTION_BUCKET,
        info.conversation_id ?? UNASSIGNED_INTERRUPTION_BUCKET
      );
      setConversationInterruptionCounts(prev => {
        const existing = prev.get(convKey);
        if (!existing) return prev;
        const next = new Map(prev);
        const newTotal = Math.max(0, existing.total - 1);
        const newBySev = { ...existing.by_severity, [sev]: Math.max(0, existing.by_severity[sev] - 1) };
        const newTypes = existing.types.map(t =>
          t.severity === info.severity && t.interruption_type === info.interruption_type
            ? { ...t, count: Math.max(0, t.count - 1) }
            : t
        ).filter(t => t.count > 0);
        if (newTotal === 0) {
          next.delete(convKey);
        } else {
          next.set(convKey, { ...existing, total: newTotal, by_severity: newBySev, types: newTypes });
        }
        return next;
      });
    }

    // 4. Update session-level badge counts (same unassigned handling).
    {
      const sessKey = info.session_id ?? UNASSIGNED_INTERRUPTION_BUCKET;
      setSessionInterruptionCounts(prev => {
        const existing = prev.get(sessKey);
        if (!existing) return prev;
        const next = new Map(prev);
        const newTotal = Math.max(0, existing.total - 1);
        const newBySev = { ...existing.by_severity, [sev]: Math.max(0, existing.by_severity[sev] - 1) };
        const newTypes = existing.types.map(t =>
          t.severity === info.severity && t.interruption_type === info.interruption_type
            ? { ...t, count: Math.max(0, t.count - 1) }
            : t
        ).filter(t => t.count > 0);
        if (newTotal === 0) {
          next.delete(sessKey);
        } else {
          next.set(sessKey, { ...existing, total: newTotal, by_severity: newBySev, types: newTypes });
        }
        return next;
      });
    }
  }, []);

  // Sync filter state to URL so back-navigation restores it
  const syncParams = useCallback((sMs: number, eMs: number, agent: string) => {
    const p: Record<string, string> = {
      start: String(sMs),
      end: String(eMs),
      q: '1',
    };
    if (agent) p.agent = agent;
    setSearchParams(p, { replace: true });
  }, [setSearchParams]);

  // Load agent names whenever time range changes (for dropdown options)
  const loadAgentNames = useCallback(async (sMs: number, eMs: number) => {
    setAgentNamesLoading(true);
    try {
      const names = await fetchAgentNames(sMs * 1_000_000, eMs * 1_000_000);
      setAgentNames(names);
      // If currently selected agent is no longer in list, reset
      setSelectedAgent((prev) => (names.includes(prev) ? prev : ''));
    } catch {
      // silently ignore — agent name list is best-effort
    } finally {
      setAgentNamesLoading(false);
    }
  }, []);

  // Load agent names on mount and on time range changes
  useEffect(() => {
    loadAgentNames(startMs, endMs);
  }, [startMs, endMs, loadAgentNames]);

  // Shared data-fetch helper: runs all 7 parallel queries and updates state.
  const runQuery = useCallback(async (startNs: number, endNs: number, agent?: string) => {
    const [sessData, tsData, intData, iStats, iSessionCounts, iConvCounts, savingsResp] = await Promise.all([
      fetchSessions(startNs, endNs).then((data) =>
        agent ? data.filter((s) => s.agent_name === agent) : data
      ),
      fetchTimeseries(startNs, endNs, agent),
      fetchInterruptionCount(startNs, endNs, agent).catch(() => null),
      fetchInterruptionStats(startNs, endNs).catch(() => [] as InterruptionTypeStat[]),
      fetchInterruptionSessionCounts(startNs, endNs, agent).catch(() => [] as SessionInterruptionCount[]),
      fetchInterruptionConversationCounts(startNs, endNs, agent).catch(() => [] as ConversationInterruptionCount[]),
      fetchTokenSavings(startNs, endNs, agent).catch(() => null),
    ]);
    setSessions(sessData);
    setTokenSeries(tsData.token_series);
    setModelSeries(tsData.model_series);
    setInterruptionCount(intData);
    setInterruptionStats(iStats);
    setSessionInterruptionCounts(new Map(iSessionCounts.map((c) => [c.session_id, c])));
    setConversationInterruptionCounts(new Map(
      iConvCounts.map((c) => [conversationInterruptionKey(c.session_id, c.conversation_id), c])
    ));
    setSavingsMap(new Map(
      savingsResp?.sessions.map((s) => [s.session_id, s.compounded_saved ?? s.saved_tokens]) ?? []
    ));
  }, []);

  const handleQuery = useCallback(async () => {
    const effectiveEnd = Date.now();
    setEndMs(effectiveEnd);
    setLoading(true);
    setTimeseriesLoading(true);
    setError(null);
    setHasQueried(true);
    setSessionPage(0); // reset to first page on new query

    const startNs = startMs * 1_000_000;
    const endNs = effectiveEnd * 1_000_000;
    const agent = selectedAgent || undefined;
    setQueryRangeNs([startNs, endNs]);
    syncParams(startMs, effectiveEnd, selectedAgent);

    try {
      await runQuery(startNs, endNs, agent);
    } catch (e: any) {
      setError(e.message ?? t('cl.queryFailed'));
    } finally {
      setLoading(false);
      setTimeseriesLoading(false);
    }
  }, [startMs, selectedAgent, syncParams, runQuery, t]);

  // Auto-load on mount: show all records for the default time range immediately
  const hasRestoredRef = React.useRef(false);
  useEffect(() => {
    if (!hasRestoredRef.current) {
      hasRestoredRef.current = true;
      const startNs = initStart * 1_000_000;
      const endNs = initEnd * 1_000_000;
      const agent = initAgent || undefined;
      setHasQueried(true);
      setLoading(true);
      setTimeseriesLoading(true);
      setQueryRangeNs([startNs, endNs]);
      runQuery(startNs, endNs, agent).catch((e: any) => {
        setError(e.message ?? t('cl.queryFailed'));
      }).finally(() => {
        setLoading(false);
        setTimeseriesLoading(false);
      });
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const totalInputTokens = sessions.reduce((s, x) => s + x.total_input_tokens, 0);
  const totalOutputTokens = sessions.reduce((s, x) => s + x.total_output_tokens, 0);

  return (
    <>
      <main className="max-w-screen-xl mx-auto px-6 py-6 space-y-6">
        {/* ── Filter bar ── */}
        <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-end gap-4">
          {/* Time range */}
          <DateTimePicker label={t('common.startTime')} value={startMs} onChange={setStartMs} />
          <DateTimePicker label={t('common.endTime')} value={endMs} onChange={setEndMs} />

          {/* Quick presets */}
          <div className="flex gap-2 flex-wrap">
            {[
              { label: t('common.last1h'), ms: 3600 * 1000 },
              { label: t('common.last6h'), ms: 6 * 3600 * 1000 },
              { label: t('common.last24h'), ms: 24 * 3600 * 1000 },
              { label: t('common.last7d'), ms: 7 * 24 * 3600 * 1000 },
            ].map(({ label, ms }) => (
              <button
                key={label}
                onClick={() => {
                  const n = Date.now();
                  setEndMs(n);
                  setStartMs(n - ms);
                }}
                className="px-3 py-1.5 text-xs bg-gray-100 hover:bg-gray-200 rounded-lg text-gray-600 transition-colors"
              >
                {label}
              </button>
            ))}
          </div>

          {/* Agent name selector */}
          <div className="flex items-center gap-2">
            <label className="text-sm text-gray-600 whitespace-nowrap">{t('common.agent')}</label>
            <select
              className="border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400 min-w-[160px]"
              value={selectedAgent}
              onChange={(e) => setSelectedAgent(e.target.value)}
              disabled={agentNamesLoading}
            >
              <option value="">{t('common.allAgents')}</option>
              {agentNames.map((n) => (
                <option key={n} value={n}>{n}</option>
              ))}
            </select>
            {agentNamesLoading && (
              <span className="text-xs text-gray-400">{t('common.loading')}</span>
            )}
          </div>

          {/* Query button */}
          <button
            onClick={handleQuery}
            disabled={loading}
            className="ml-auto px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 disabled:opacity-50 transition-colors"
          >
            {loading ? t('common.querying') : t('common.query')}
          </button>
        </div>

        {/* ── Error banner ── */}
        {error && (
          <div className="p-4 bg-red-50 border border-red-200 rounded-xl text-red-600 text-sm">
            ⚠️ {error}
          </div>
        )}

        {/* ── Content shown only after first query ── */}
        {hasQueried && (
          <>
            {/* Summary cards */}
            <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <p className="text-sm text-gray-500">{t('cl.sessions')}</p>
                <p className="text-3xl font-bold text-gray-900 mt-1">{sessions.length}</p>
              </div>
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <p className="text-sm text-gray-500">{t('cl.totalInputTokens')}</p>
                <p className="text-3xl font-bold text-blue-600 mt-1">
                  {fmtTokens(totalInputTokens)}
                </p>
              </div>
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <p className="text-sm text-gray-500">{t('cl.totalOutputTokens')}</p>
                <p className="text-3xl font-bold text-green-600 mt-1">
                  {fmtTokens(totalOutputTokens)}
                </p>
              </div>
              {/* ── Interruption card ── */}
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <p className="text-sm text-gray-500">{t('cl.interruptions')}</p>
                {interruptionCount === null ? (
                  <p className="text-3xl font-bold text-gray-400 mt-1">—</p>
                ) : (
                  <>
                    <p className="text-3xl font-bold text-red-500 mt-1">{interruptionCount.total}</p>
                    {interruptionCount.total > 0 && (
                      <div className="flex flex-wrap gap-1.5 mt-2">
                        {(
                          [
                            { key: 'critical', label: t('common.critical'), bg: 'bg-red-100 text-red-700 border border-red-300' },
                            { key: 'high',     label: t('common.high'), bg: 'bg-orange-100 text-orange-700 border border-orange-300' },
                            { key: 'medium',   label: t('common.medium'), bg: 'bg-yellow-100 text-yellow-700 border border-yellow-300' },
                            { key: 'low',      label: t('common.low'), bg: 'bg-blue-100 text-blue-700 border border-blue-300' },
                          ] as const
                        ).map(({ key, label, bg }) => {
                          const cnt = interruptionCount.by_severity[key];
                          if (cnt === 0) return null;
                          const tooltipLines = interruptionStats
                            .filter((s) => s.severity === key)
                            .sort((a, b) => b.count - a.count)
                            .map((s) => `${s.interruption_type}: ${s.count}`);
                          return (
                            <span
                              key={key}
                              className={`relative group inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs font-semibold cursor-default ${bg}`}
                            >
                              {label} {cnt}
                              {tooltipLines.length > 0 && (
                                <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 hidden group-hover:flex flex-col items-start px-2 py-1.5 rounded bg-gray-800 text-white text-xs whitespace-nowrap shadow-lg z-50 pointer-events-none">
                                  {tooltipLines.map((line, i) => (
                                    <span key={i}>{line}</span>
                                  ))}
                                  <span className="absolute top-full left-1/2 -translate-x-1/2 border-4 border-transparent border-t-gray-800" />
                                </span>
                              )}
                            </span>
                          );
                        })}
                      </div>
                    )}
                  </>
                )}
              </div>
            </div>

            {/* ── Time-series charts ── */}
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
              {/* Token time-series */}
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <h2 className="text-sm font-semibold text-gray-700 mb-3">{t('cl.tokenTimeseries')}</h2>
                {timeseriesLoading ? (
                  <div className="flex items-center justify-center h-32 text-gray-400 text-sm">{t('common.loading')}</div>
                ) : (
                  <TokenTimeseriesChart data={tokenSeries} startNs={queryRangeNs[0]} endNs={queryRangeNs[1]} />
                )}
              </div>

              {/* Model token time-series */}
              <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
                <h2 className="text-sm font-semibold text-gray-700 mb-3">{t('cl.modelTokenTimeseries')}</h2>
                {timeseriesLoading ? (
                  <div className="flex items-center justify-center h-32 text-gray-400 text-sm">{t('common.loading')}</div>
                ) : (
                  <ModelTimeseriesChart data={modelSeries} startNs={queryRangeNs[0]} endNs={queryRangeNs[1]} />
                )}
              </div>
            </div>

            {/* ── Session table ── */}
            <div className="bg-white rounded-xl shadow-sm border border-gray-200 overflow-hidden">
              <div className="overflow-x-auto">
                <table className="w-full min-w-[900px]">
                  <thead className="bg-gray-50 border-b border-gray-200">
                    <tr>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[220px]">
                        <span className="inline-flex items-center gap-1.5">
                          <span>{t('cl.sessionId')}</span>
                          <SessionIdHelp />
                        </span>
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[120px]">
                        {t('cl.agent')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[100px]">
                        {t('cl.model')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[80px]">
                        {t('cl.conversations')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[110px]">
                        {t('cl.inputTokens')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[100px]">
                        {t('cl.savedTokens')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[110px]">
                        {t('cl.outputTokens')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[150px]">
                        {t('cl.lastActive')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[80px]">
                        {t('cl.actions')}
                      </th>
                      <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[100px]">
                        {t('cl.interrupts')}
                      </th>
                    </tr>
                  </thead>
                <tbody className="divide-y divide-gray-100">
                  {/* Interruptions the backend could not attribute to a session.
                      Rendered outside the pagination slice so the visible
                      breakdown always adds up to the overview total. */}
                  {(() => {
                    const ic = sessionInterruptionCounts.get(UNASSIGNED_INTERRUPTION_BUCKET);
                    if (!ic || ic.total === 0) return null;
                    return (
                      <tr className="bg-amber-50">
                        <td className="px-4 lg:px-6 py-4">
                          <span
                            className="text-sm text-amber-800"
                            title={t('cl.unassignedSessionHint')}
                          >
                            {t('cl.unassignedSession')}
                          </span>
                        </td>
                        <td colSpan={8} className="px-4 lg:px-6 py-4 text-xs text-amber-700">
                          {t('cl.unassignedSessionHint')}
                        </td>
                        <td className="px-4 lg:px-6 py-4">
                          <InterruptionBadge bySeverity={ic.by_severity} types={ic.types} />
                        </td>
                      </tr>
                    );
                  })()}
                  {!loading && sessions.length === 0 && (
                    <tr>
                      <td colSpan={10} className="px-4 lg:px-6 py-12 text-center text-gray-400">
                        <div className="text-4xl mb-2">🔍</div>
                        <p>{t('cl.noSessions')}</p>
                        <p className="text-xs mt-1">{t('cl.ensureServiceRunning')}</p>
                      </td>
                    </tr>
                  )}

                  {(() => {
                    const sessionTotalPages = Math.max(1, Math.ceil(sessions.length / SESSION_PAGE_SIZE));
                    const pageSessions = sessions.slice(sessionPage * SESSION_PAGE_SIZE, (sessionPage + 1) * SESSION_PAGE_SIZE);
                    return pageSessions.map((sess) => {
                    const isExpanded = expandedSession === sess.session_id;
                    return (
                      <React.Fragment key={sess.session_id}>
                        {/* Session row */}
                        <tr
                          className={`hover:bg-gray-50 transition-colors cursor-pointer ${
                            isExpanded ? 'bg-blue-50' : ''
                          }`}
                          onClick={() =>
                            setExpandedSession(isExpanded ? null : sess.session_id)
                          }
                        >
                          <td className="px-4 lg:px-6 py-4">
                            <div className="flex items-center gap-2">
                              <span className="text-gray-400 text-xs flex-shrink-0">
                                {isExpanded ? '▼' : '▶'}
                              </span>
                              <span
                                className="font-mono text-sm text-gray-800 truncate"
                                title={sess.session_id}
                              >
                                {shortId(sess.session_id, 20)}
                              </span>
                              <CopyButton text={sess.session_id} />
                            </div>
                          </td>
                          <td className="px-4 lg:px-6 py-4 text-sm text-gray-700">
                            <span className="truncate block" title={sess.agent_name ?? ''}>
                              {sess.agent_name ?? <span className="text-gray-400">—</span>}
                            </span>
                          </td>
                          <td className="px-4 lg:px-6 py-4">
                            {sess.model ? (
                              <span className="px-2 py-0.5 bg-purple-100 text-purple-800 rounded-full text-xs font-medium truncate max-w-[80px] block" title={sess.model}>
                                {sess.model}
                              </span>
                            ) : (
                              <span className="text-gray-400 text-sm">—</span>
                            )}
                          </td>
                          <td className="px-4 lg:px-6 py-4 text-sm text-gray-700">
                            {sess.conversation_count}
                          </td>
                          <td className="px-4 lg:px-6 py-4 text-sm font-semibold text-blue-600">
                            {fmtTokens(sess.total_input_tokens)}
                          </td>
                          <td className="px-4 lg:px-6 py-4" onClick={(e) => e.stopPropagation()}>
                            {(() => {
                              const saved = savingsMap.get(sess.session_id);
                              if (!saved) return <span className="text-xs text-gray-300">—</span>;
                              const params = new URLSearchParams({
                                session_id: sess.session_id,
                                start: String(startMs),
                                end: String(endMs),
                              });
                              if (selectedAgent) params.set('agent', selectedAgent);
                              return (
                                <a
                                  href={`#/savings?${params.toString()}`}
                                  className="text-sm font-semibold text-green-600 underline decoration-green-300 hover:text-green-800 hover:decoration-green-600 transition-colors"
                                >
                                  {fmtTokens(saved)}
                                </a>
                              );
                            })()}
                          </td>
                          <td className="px-4 lg:px-6 py-4 text-sm font-semibold text-green-600">
                            {fmtTokens(sess.total_output_tokens)}
                          </td>
                          <td className="px-4 lg:px-6 py-4 text-xs text-gray-500">
                            {nsToDate(sess.last_seen_ns, locale)}
                          </td>
                          <td className="px-4 lg:px-6 py-4">
                            <a
                              href={`#/atif?type=session&id=${encodeURIComponent(sess.session_id)}`}
                              onClick={(e) => e.stopPropagation()}
                              className="px-3 py-1 bg-white border border-blue-300 text-blue-700 rounded-lg text-xs hover:bg-blue-50 transition-colors whitespace-nowrap"
                            >
                              {t('common.details')}
                            </a>
                          </td>
                          <td className="px-4 lg:px-6 py-4" onClick={(e) => e.stopPropagation()}>
                            {(() => {
                              const ic = sessionInterruptionCounts.get(sess.session_id);
                              if (!ic || ic.total === 0) return <span className="text-xs text-gray-300">—</span>;
                              return (
                                <InterruptionBadge
                                  bySeverity={ic.by_severity}
                                  types={ic.types}
                                />
                              );
                            })()}
                          </td>
                        </tr>

                        {/* Expanded trace sub-table */}
                        {isExpanded && (
                          <TraceSubTable
                            key={`${sess.session_id}-${queryRangeNs[0]}`}
                            sessionId={sess.session_id}
                            conversationInterruptionCounts={conversationInterruptionCounts}
                            startNs={queryRangeNs[0]}
                            endNs={queryRangeNs[1]}
                            onResolvedEvent={handleResolvedEvent}
                          />
                        )}
                      </React.Fragment>
                    );
                  });
                  })()}
                </tbody>
                </table>
              </div>
              {/* Session pagination controls */}
              {sessions.length > SESSION_PAGE_SIZE && (() => {
                const sessionTotalPages = Math.ceil(sessions.length / SESSION_PAGE_SIZE);
                return (
                  <div className="flex items-center gap-2 justify-end px-4 py-3 border-t border-gray-100">
                    <span className="text-xs text-gray-500">
                      {sessionPage * SESSION_PAGE_SIZE + 1}–{Math.min((sessionPage + 1) * SESSION_PAGE_SIZE, sessions.length)} / {sessions.length}
                    </span>
                    <button
                      onClick={() => setSessionPage((p) => Math.max(0, p - 1))}
                      disabled={sessionPage === 0}
                      className="px-2 py-0.5 text-xs border border-gray-300 text-gray-600 rounded hover:bg-gray-50 disabled:opacity-40 transition-colors"
                    >
                      &lsaquo; {t('common.prev')}
                    </button>
                    {Array.from({ length: sessionTotalPages }, (_, i) => (
                      <button
                        key={i}
                        onClick={() => setSessionPage(i)}
                        className={`px-2 py-0.5 text-xs rounded transition-colors ${
                          i === sessionPage
                            ? 'bg-blue-600 text-white'
                            : 'border border-gray-300 text-gray-600 hover:bg-gray-50'
                        }`}
                      >
                        {i + 1}
                      </button>
                    ))}
                    <button
                      onClick={() => setSessionPage((p) => Math.min(sessionTotalPages - 1, p + 1))}
                      disabled={sessionPage === sessionTotalPages - 1}
                      className="px-2 py-0.5 text-xs border border-gray-300 text-gray-600 rounded hover:bg-gray-50 disabled:opacity-40 transition-colors"
                    >
                      {t('common.next')} &rsaquo;
                    </button>
                  </div>
                );
              })()}
            </div>
          </>
        )}

      </main>
    </>
  );
};
