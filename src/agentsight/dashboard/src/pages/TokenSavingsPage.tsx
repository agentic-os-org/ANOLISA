import React, { useState, useRef, useEffect, useCallback } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  PieChart, Pie, Cell, ResponsiveContainer,
} from 'recharts';
import { fetchTokenSavings, fetchAgentNames } from '../utils/apiClient';
import type { SessionSavings, SavingsSummary, OptimizationItem, DiffLine, StrategyBreakdownItem, OptimizationTip } from '../utils/apiClient';
import { DateTimePicker } from '../components/DateTimePicker';
import { SessionIdHelp } from '../components/SessionIdHelp';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';

// ─── Helpers ──────────────────────────────────────────────────────────────────

function fmtTokens(n: number, locale: string): string {
  return n.toLocaleString(locale);
}

/** Info tooltip: stays visible while the pointer is over the tooltip */
const InfoTooltip: React.FC<{ text: string }> = ({ text }) => {
  const [show, setShow] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const open = () => { if (timer.current) clearTimeout(timer.current); setShow(true); };
  const close = () => { timer.current = setTimeout(() => setShow(false), 120); };
  useEffect(() => () => { if (timer.current) clearTimeout(timer.current); }, []);
  return (
    <span
      className="relative inline-flex items-center ml-1"
      onMouseEnter={open}
      onMouseLeave={close}
    >
      <span className="w-4 h-4 rounded-full bg-gray-200 text-gray-500 text-[10px] font-bold flex items-center justify-center cursor-default">
        i
      </span>
      {show && (
        <span className="absolute bottom-full left-0 mb-1 px-3 py-2 bg-gray-800 text-white text-xs rounded-lg z-50 shadow-lg w-[300px] leading-relaxed">
          {text}
        </span>
      )}
    </span>
  );
};

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

// ─── Types ────────────────────────────────────────────────────────────────────

type OptimizationCategory = 'tool_output' | 'mcp_response';

// ─── Category config ──────────────────────────────────────────────────────────

const CATEGORY_CONFIG: Record<OptimizationCategory, { labelKey: MessageKey; color: string; bg: string }> = {
  tool_output: { labelKey: 'ts.toolOutput', color: 'text-orange-700', bg: 'bg-orange-100' },
  mcp_response: { labelKey: 'ts.mcpOutput', color: 'text-violet-700', bg: 'bg-violet-100' },
};

// ─── Strategy config ─────────────────────────────────────────────────────────

const STRATEGY_CONFIG: Record<string, { labelKey: MessageKey; color: string; bg: string; pie: string; tooltipKey: MessageKey }> = {
  'compress-schema':   { labelKey: 'ts.schemaCompression', color: 'text-blue-700',   bg: 'bg-blue-100',   pie: '#3b82f6', tooltipKey: 'ts.schemaCompressionTip' },
  'compress-response': { labelKey: 'ts.responseCompression', color: 'text-violet-700', bg: 'bg-violet-100', pie: '#8b5cf6', tooltipKey: 'ts.responseCompressionTip' },
  'rewrite-command':   { labelKey: 'ts.commandRewrite', color: 'text-orange-700', bg: 'bg-orange-100', pie: '#f59e0b', tooltipKey: 'ts.commandRewriteTip' },
  'compress-toon':     { labelKey: 'ts.toonEncoding', color: 'text-teal-700',  bg: 'bg-teal-100',  pie: '#14b8a6', tooltipKey: 'ts.toonEncodingTip' },
};

// ─── Pie chart data ───────────────────────────────────────────────────────────

const PIE_COLORS = ['#3b82f6', '#10b981']; // input blue, output green
const SAVED_PIE_COLORS = ['#f59e0b', '#8b5cf6']; // tool orange, MCP violet

// ─── Time range presets ───────────────────────────────────────────────────────

const TIME_PRESETS: { labelKey: MessageKey; ms: number }[] = [
  { labelKey: 'common.last1h', ms: 3600 * 1000 },
  { labelKey: 'common.last6h', ms: 6 * 3600 * 1000 },
  { labelKey: 'common.last24h', ms: 24 * 3600 * 1000 },
  { labelKey: 'common.last7d', ms: 7 * 24 * 3600 * 1000 },
];

// ─── Diff view (split / unified toggle) ──────────────────────────────────────

const DiffView: React.FC<{ item: OptimizationItem }> = ({ item }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const diffLines = item.diff_lines ?? [];
  const addedCount = diffLines.filter(l => l.type === 'add').length;
  const removedCount = diffLines.filter(l => l.type === 'remove').length;

  const diffLineClass = (type: string): string => {
    switch (type) {
      case 'add': return 'bg-green-50 text-green-800';
      case 'remove': return 'bg-red-50 text-red-800 line-through';
      case 'separator': return 'bg-gray-100 text-gray-400 text-center';
      default: return 'text-gray-700';
    }
  };

  const diffLineMarker = (type: string): string => {
    switch (type) {
      case 'add': return '+';
      case 'remove': return '-';
      case 'separator': return ' ';
      default: return ' ';
    }
  };

  return (
    <div className="bg-white rounded-lg border border-gray-200 overflow-hidden">
      {/* Explanation banner */}
      <div className="px-3 py-2 bg-blue-50 border-b border-blue-100 flex items-start gap-2">
        <span className="text-blue-500 mt-0.5">💡</span>
        <div>
          <p className="text-sm text-blue-800 font-medium">{item.explanation}</p>
          <p className="text-xs text-blue-600 mt-0.5">
            {t('ts.compressionRatio')} <span className="font-semibold">{item.compression_ratio.toFixed(1)}%</span>
            {' · '}
            {t('ts.affectsCalls', { n: item.compounding_turns })}
            {' · '}
            {t('ts.compoundedSavings', { n: fmtTokens(item.compounded_saved, locale) })}
          </p>
        </div>
      </div>

      {/* Line-level diff body */}
      <div className="overflow-x-auto max-h-[400px] overflow-y-auto">
        {diffLines.length > 0 ? (
          <table className="w-full font-mono text-xs border-collapse">
            <tbody>
              {diffLines.map((line, idx) => (
                <tr key={idx} className={diffLineClass(line.type)}>
                  <td className="px-2 py-0.5 select-none w-6 text-right opacity-50">
                    {line.type !== 'separator' ? idx + 1 : ''}
                  </td>
                  <td className="px-1 py-0.5 select-none w-4 font-bold">
                    {diffLineMarker(line.type)}
                  </td>
                  <td className="px-2 py-0.5 whitespace-pre-wrap break-all">
                    {line.content}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <div className="grid grid-cols-2 divide-x divide-gray-200">
            <div>
              <div className="px-2 py-1 text-xs font-semibold text-red-600 bg-red-50 border-b border-gray-200">{t('common.original')}</div>
              <pre className="font-mono text-xs px-2 py-1 break-all whitespace-pre-wrap bg-red-50 text-red-700">
                {item.before_text || t('common.noChange')}
              </pre>
            </div>
            <div>
              <div className="px-2 py-1 text-xs font-semibold text-green-600 bg-green-50 border-b border-gray-200">{t('common.optimized')}</div>
              <pre className="font-mono text-xs px-2 py-1 break-all whitespace-pre-wrap bg-green-50 text-green-700">
                {item.after_text || t('common.noChange')}
              </pre>
            </div>
          </div>
        )}
      </div>

      {/* Stats footer */}
      {diffLines.length > 0 && (
        <div className="px-4 py-2 bg-gray-50 border-t border-gray-200 flex gap-4 text-xs text-gray-500">
          <span className="text-red-600">{t('common.linesRemoved', { n: removedCount })}</span>
          <span className="text-green-600">{t('common.linesAdded', { n: addedCount })}</span>
        </div>
      )}
    </div>
  );
};

// ─── Optimization Tips Panel ─────────────────────────────────────────────────

const TIP_STYLE: Record<string, { icon: string; border: string; bg: string; text: string }> = {
  success: { icon: '✅', border: 'border-green-200', bg: 'bg-green-50', text: 'text-green-800' },
  info: { icon: '💡', border: 'border-blue-200', bg: 'bg-blue-50', text: 'text-blue-800' },
  warning: { icon: '⚠️', border: 'border-yellow-200', bg: 'bg-yellow-50', text: 'text-yellow-800' },
};

const OptimizationTipsPanel: React.FC<{ tips: OptimizationTip[] }> = ({ tips }) => {
  const { t } = useI18n();
  if (tips.length === 0) return null;
  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
      <h3 className="text-sm font-semibold text-gray-700 mb-3 flex items-center gap-2">
        {t('ts.optimizationTips')}
      </h3>
      <div className="space-y-2">
        {tips.map((tip, idx) => {
          const style = TIP_STYLE[tip.level] || TIP_STYLE.info;
          return (
            <div key={idx} className={`flex items-start gap-2 px-3 py-2 rounded-lg border ${style.border} ${style.bg}`}>
              <span className="mt-0.5">{style.icon}</span>
              <div>
                <p className={`text-sm font-medium ${style.text}`}>{tip.title}</p>
                <p className={`text-xs ${style.text} opacity-80 mt-0.5`}>{tip.description}</p>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
};

// ─── Savings Breakdown Panel ─────────────────────────────────────────────────

const SavingsBreakdownPanel: React.FC<{ sessions: SessionSavings[] }> = ({ sessions }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  // Get top 5 optimization items across all sessions by compounded_saved
  const allItems = sessions.flatMap(s =>
    s.optimization_items.map(item => ({
      ...item,
      session_id: s.session_id,
      agent_name: s.agent_name,
    }))
  );
  const topItems = [...allItems]
    .sort((a, b) => b.compounded_saved - a.compounded_saved)
    .slice(0, 5);

  if (topItems.length === 0) return null;

  const maxSaved = topItems[0]?.compounded_saved || 1;

  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
      <h3 className="text-sm font-semibold text-gray-700 mb-3 flex items-center gap-2">
        {t('ts.savingsTop5')}
      </h3>
      <div className="space-y-2">
        {topItems.map((item, idx) => {
          const cfg = CATEGORY_CONFIG[item.category];
          const pct = (item.compounded_saved / maxSaved) * 100;
          return (
            <div key={item.id || idx} className="flex items-center gap-3">
              <span className="text-xs text-gray-400 w-4 text-right">#{idx + 1}</span>
              <span className={`px-1.5 py-0.5 rounded text-xs font-medium ${cfg.bg} ${cfg.color} flex-shrink-0`}>
                {t(cfg.labelKey)}
              </span>
              <div className="flex-1 min-w-0">
                <div className="h-5 bg-gray-100 rounded-full overflow-hidden relative">
                  <div
                    className="h-full bg-gradient-to-r from-green-400 to-green-600 rounded-full transition-all"
                    style={{ width: `${pct}%` }}
                  />
                  <span className="absolute inset-0 flex items-center px-2 text-xs font-medium text-gray-700">
                    {t('common.tokens', { n: fmtTokens(item.compounded_saved, locale) })}
                  </span>
                </div>
              </div>
              <span className="text-xs text-gray-400 flex-shrink-0 truncate max-w-[100px]" title={item.agent_name}>
                {item.agent_name}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
};

// ─── Optimization table row ───────────────────────────────────────────────────

const OptimizationTableRow: React.FC<{ item: OptimizationItem }> = ({ item }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [expanded, setExpanded] = useState(false);
  const cfg = CATEGORY_CONFIG[item.category];
  const stratConfig = STRATEGY_CONFIG[item.strategy];
  const stratStyle = stratConfig ?? { color: 'text-gray-700', bg: 'bg-gray-100', pie: '#9ca3af' };
  const stratLabel = stratConfig ? t(stratConfig.labelKey) : (item.strategy_label || item.strategy);
  const stratTooltip = stratConfig ? t(stratConfig.tooltipKey) : '';
  const savingsPercent = item.before_tokens > 0
    ? ((item.before_tokens - item.after_tokens) / item.before_tokens * 100).toFixed(0)
    : '0';

  return (
    <>
      <tr className="hover:bg-gray-50 transition-colors">
        <td className="px-4 py-3">
          <span className={`px-2 py-0.5 rounded text-xs font-medium ${cfg.bg} ${cfg.color}`}>
            {t(cfg.labelKey)}
          </span>
          <span className="ml-2 text-xs text-gray-500">{item.title}</span>
        </td>
        <td className="px-4 py-3">
          <span className={`relative group px-2 py-0.5 rounded text-xs font-medium ${stratStyle.bg} ${stratStyle.color} cursor-default`}>
            {stratLabel}
            {stratTooltip && (
              <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1.5 hidden group-hover:block px-2 py-1.5 rounded bg-gray-800 text-white text-xs whitespace-nowrap shadow-lg z-50 pointer-events-none">
                {stratTooltip}
                <span className="absolute top-full left-1/2 -translate-x-1/2 border-4 border-transparent border-t-gray-800" />
              </span>
            )}
          </span>
        </td>
        <td className="px-4 py-3 text-sm text-gray-600 text-right">
          {fmtTokens(item.before_tokens, locale)}
        </td>
        <td className="px-4 py-3 text-sm text-gray-600 text-right">
          {fmtTokens(item.after_tokens, locale)}
        </td>
        <td className="px-4 py-3 text-sm font-semibold text-green-600 text-right">
          {fmtTokens(item.compounded_saved, locale)}
          <span className="text-xs text-gray-400 ml-1">{t('ts.singleTurn', { pct: savingsPercent })}</span>
        </td>
        <td className="px-4 py-3 text-center">
          <button
            onClick={() => setExpanded(!expanded)}
            className="text-xs text-blue-600 hover:text-blue-800 transition-colors"
          >
            {expanded ? t('common.collapse') : t('common.details')}
          </button>
        </td>
      </tr>
      {expanded && (
        <tr className="bg-gray-50">
          <td colSpan={6} className="px-4 py-3">
            <DiffView item={item} />
          </td>
        </tr>
      )}
    </>
  );
};

// ─── Session row with expand ──────────────────────────────────────────────────

const SessionRow: React.FC<{
  session: SessionSavings;
  initialExpanded?: boolean;
  rowRef?: React.Ref<HTMLTableRowElement>;
}> = ({ session, initialExpanded = false, rowRef }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [expanded, setExpanded] = useState(initialExpanded);

  return (
    <>
      <tr
        ref={rowRef}
        className={`hover:bg-gray-50 transition-colors cursor-pointer ${
          expanded ? 'bg-blue-50' : ''
        }`}
        onClick={() => setExpanded(!expanded)}
      >
        <td className="px-4 lg:px-6 py-4">
          <div className="flex items-center gap-2">
            <span className="text-gray-400 text-xs flex-shrink-0">
              {expanded ? '▼' : '▶'}
            </span>
            <span
              className="font-mono text-sm text-gray-800 truncate"
              title={session.session_id}
            >
              {shortId(session.session_id, 20)}
            </span>
            <CopyButton text={session.session_id} />
          </div>
        </td>
        <td className="px-4 lg:px-6 py-4 text-sm text-gray-700">
          <span className="truncate block" title={session.agent_name}>
            {session.agent_name}
          </span>
        </td>
        <td className="px-4 lg:px-6 py-4 text-sm text-gray-900 text-right">
          {fmtTokens(session.total_input_tokens, locale)}
        </td>
        <td className="px-4 lg:px-6 py-4 text-sm text-gray-900 text-right">
          {fmtTokens(session.total_output_tokens, locale)}
        </td>
        <td className="px-4 lg:px-6 py-4 text-sm font-semibold text-green-600 text-right">
          {fmtTokens(session.compounded_saved, locale)}
        </td>
        <td className="px-4 lg:px-6 py-4">
          <div className="flex items-center gap-2">
            <div className="flex-1 h-2 bg-gray-200 rounded-full overflow-hidden max-w-[80px]">
              <div
                className="h-full bg-green-500 rounded-full"
                style={{ width: `${Math.min(session.compounded_savings_rate * 100, 100)}%` }}
              />
            </div>
            <span className="text-xs font-semibold text-green-600">
              {(session.compounded_savings_rate * 100).toFixed(1)}%
            </span>
          </div>
        </td>
      </tr>

      {/* Expanded detail */}
      {expanded && (
        <tr className="bg-blue-50">
          <td colSpan={6} className="px-4 lg:px-8 py-4">
            {/* Optimization items table */}
            <div className="bg-white rounded-lg border border-gray-200 overflow-hidden">
              <table className="w-full min-w-[700px]">
                <thead className="bg-gray-50 border-b border-gray-200">
                  <tr>
                    <th className="px-4 py-2 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[90px]">
                      {t('ts.category')}
                    </th>
                    <th className="px-4 py-2 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide w-[110px]">
                      {t('ts.savingsStrategy')}
                    </th>
                    <th className="px-4 py-2 text-right text-xs font-semibold text-gray-600 uppercase tracking-wide w-[100px]">
                      {t('ts.before')}
                    </th>
                    <th className="px-4 py-2 text-right text-xs font-semibold text-gray-600 uppercase tracking-wide w-[100px]">
                      {t('ts.optimizedCol')}
                    </th>
                    <th className="px-4 py-2 text-right text-xs font-semibold text-gray-600 uppercase tracking-wide w-[100px]">
                      {t('ts.savedCol')}
                    </th>
                    <th className="px-4 py-2 text-center text-xs font-semibold text-gray-600 uppercase tracking-wide w-[60px]">
                      {t('ts.detailsCol')}
                    </th>
                  </tr>
                </thead>
                <tbody className="divide-y divide-gray-100">
                  {session.optimization_items.map((item) => (
                    <OptimizationTableRow key={item.id} item={item} />
                  ))}
                </tbody>
              </table>
            </div>
          </td>
        </tr>
      )}
    </>
  );
};

// ─── Main page ────────────────────────────────────────────────────────────────

export const TokenSavingsPage: React.FC = () => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [searchParams] = useSearchParams();
  const now = Date.now();

  // Read URL params for deep-link from homepage
  const targetSessionId = searchParams.get('session_id');
  const paramStart = Number(searchParams.get('start'));
  const paramEnd = Number(searchParams.get('end'));
  const paramAgent = searchParams.get('agent') ?? '';

  const [startMs, setStartMs] = useState(paramStart || (now - 24 * 3600 * 1000));
  const [endMs, setEndMs] = useState(paramEnd || now);
  const [hasQueried, setHasQueried] = useState(false);
  const [selectedAgent, setSelectedAgent] = useState(paramAgent);

  // Track which session to auto-expand (from URL deep-link)
  const [expandedSessionId] = useState<string | null>(targetSessionId);

  // API data state
  const [sessions, setSessions] = useState<SessionSavings[]>([]);
  const [summary, setSummary] = useState<SavingsSummary | null>(null);
  const [statsAvailable, setStatsAvailable] = useState(true);
  const [tips, setTips] = useState<OptimizationTip[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [agentNames, setAgentNames] = useState<string[]>([]);

  // Ref for scrolling to the target session row
  const targetRowRef = useRef<HTMLTableRowElement>(null);

  // Load agent names on mount
  useEffect(() => {
    const startNs = (Date.now() - 7 * 24 * 3600 * 1000) * 1_000_000;
    const endNs = Date.now() * 1_000_000;
    fetchAgentNames(startNs, endNs).then(setAgentNames).catch(() => {});
  }, []);

  const handleQuery = useCallback(async () => {
    setLoading(true);
    setError(null);
    setHasQueried(true);
    try {
      const startNs = startMs * 1_000_000;
      const endNs = endMs * 1_000_000;
      const resp = await fetchTokenSavings(startNs, endNs, selectedAgent || undefined);
      setSessions(resp.sessions);
      setSummary(resp.summary);
      setStatsAvailable(resp.stats_available);
      setTips(resp.optimization_tips ?? []);
    } catch (e: any) {
      setError(e.message || t('ts.fetchFailed'));
    } finally {
      setLoading(false);
    }
  }, [startMs, endMs, selectedAgent, t]);

  // Auto-query on mount when navigated from homepage with URL params
  const hasAutoQueriedRef = useRef(false);
  useEffect(() => {
    if (targetSessionId && !hasAutoQueriedRef.current) {
      hasAutoQueriedRef.current = true;
      handleQuery();
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Auto-scroll to the target session row after data loads
  useEffect(() => {
    if (expandedSessionId && sessions.length > 0 && targetRowRef.current) {
      targetRowRef.current.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }
  }, [expandedSessionId, sessions]);

  const totalInput = summary?.total_input_tokens ?? 0;
  const totalOutput = summary?.total_output_tokens ?? 0;
  const totalTokens = summary?.total_tokens ?? 0;
  const baselineTokens = summary?.baseline_tokens ?? 0;
  const totalCompoundedSaved = summary?.total_compounded_saved ?? 0;
  const totalCompoundedToolSaved = summary?.total_compounded_tool_saved ?? 0;
  const totalCompoundedMcpSaved = summary?.total_compounded_mcp_saved ?? 0;
  const savingsRate = baselineTokens > 0 ? (totalCompoundedSaved / baselineTokens) * 100 : 0;

  return (
    <main className="max-w-screen-xl mx-auto px-6 py-6 space-y-6">
      {/* ── Filter bar ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-end gap-4">
        {/* Time range */}
        <DateTimePicker label={t('common.startTime')} value={startMs} onChange={setStartMs} />
        <DateTimePicker label={t('common.endTime')} value={endMs} onChange={setEndMs} />

        {/* Quick presets */}
        <div className="flex gap-2 flex-wrap">
          {TIME_PRESETS.map(({ labelKey, ms }) => (
            <button
              key={labelKey}
              onClick={() => {
                const n = Date.now();
                setEndMs(n);
                setStartMs(n - ms);
              }}
              className="px-3 py-1.5 text-xs bg-gray-100 hover:bg-gray-200 rounded-lg text-gray-600 transition-colors"
            >
              {t(labelKey)}
            </button>
          ))}
        </div>

        {/* Agent selector */}
        <div className="flex items-center gap-2">
          <label className="text-sm text-gray-600 whitespace-nowrap">{t('common.agent')}</label>
          <select
            className="border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400 min-w-[160px]"
            value={selectedAgent}
            onChange={(e) => setSelectedAgent(e.target.value)}
          >
            <option value="">{t('common.allAgents')}</option>
            {agentNames.map((n) => (
              <option key={n} value={n}>{n}</option>
            ))}
          </select>
        </div>

        {/* Query button */}
        <button
          onClick={handleQuery}
          disabled={loading}
          className="ml-auto px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 transition-colors disabled:opacity-50"
        >
          {loading ? t('common.querying') : t('common.query')}
        </button>
      </div>

      {/* ── Error message ── */}
      {error && (
        <div className="bg-red-50 border border-red-200 text-red-700 px-4 py-3 rounded-lg text-sm">
          {error}
        </div>
      )}

      {/* ── Stats unavailable notice ── */}
      {hasQueried && !statsAvailable && (
        <div className="bg-yellow-50 border border-yellow-200 text-yellow-700 px-4 py-3 rounded-lg text-sm">
          {t('ts.noOptimizationRecords')}
        </div>
      )}

      {/* ── Content shown only after first query ── */}
      {hasQueried && !loading ? (
      <>
      {/* ── Summary cards ── */}
      <div className="grid grid-cols-1 sm:grid-cols-3 gap-4">
        {/* Card 1: Total consumption */}
        <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
          <p className="text-sm text-gray-500 flex items-center">
            {t('ts.actualConsumption')}
            <InfoTooltip text={t('ts.actualConsumptionTip')} />
          </p>
          <p className="text-3xl font-bold text-gray-900 mt-1">{fmtTokens(totalTokens, locale)}</p>
          <div className="mt-3">
            <ResponsiveContainer width="100%" height={60}>
              <PieChart>
                <Pie
                  data={[
                    { name: t('common.input'), value: totalInput },
                    { name: t('common.output'), value: totalOutput },
                  ]}
                  cx="50%"
                  cy="50%"
                  innerRadius={14}
                  outerRadius={26}
                  paddingAngle={2}
                  dataKey="value"
                  stroke="none"
                >
                  {PIE_COLORS.map((c, i) => (
                    <Cell key={i} fill={c} />
                  ))}
                </Pie>
              </PieChart>
            </ResponsiveContainer>
            <div className="flex justify-center gap-4 -mt-1">
              <span className="flex items-center gap-1 text-xs text-gray-500">
                <span className="w-2 h-2 rounded-full bg-blue-500" />
                {t('common.input')} {fmtTokens(totalInput, locale)}
              </span>
              <span className="flex items-center gap-1 text-xs text-gray-500">
                <span className="w-2 h-2 rounded-full bg-green-500" />
                {t('common.output')} {fmtTokens(totalOutput, locale)}
              </span>
            </div>
          </div>
        </div>

        {/* Card 2: Saved tokens — strategy breakdown pie */}
        <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
          <p className="text-sm text-gray-500 flex items-center">
            {t('ts.savedTokens')}
            <InfoTooltip text={t('ts.savedTokensTip')} />
          </p>
          <p className="text-3xl font-bold text-green-600 mt-1">
            {fmtTokens(totalCompoundedSaved, locale)}
          </p>
          <p className="text-xs text-gray-400 mt-0.5">{t('ts.baseline', { n: fmtTokens(baselineTokens, locale) })}</p>
          <div className="mt-3">
            {(() => {
              const breakdown = summary?.strategy_breakdown ?? [];
              const hasStrategy = breakdown.length > 0 && breakdown.some(b => b.compounded_saved > 0);
              if (hasStrategy) {
                const pieData = breakdown
                  .filter(b => b.compounded_saved > 0)
                  .map(b => ({
                    name: (STRATEGY_CONFIG[b.strategy] ? t(STRATEGY_CONFIG[b.strategy].labelKey) : b.label),
                    value: b.compounded_saved,
                    color: (STRATEGY_CONFIG[b.strategy]?.pie ?? '#9ca3af'),
                  }));
                return (
                  <>
                    <ResponsiveContainer width="100%" height={60}>
                      <PieChart>
                        <Pie
                          data={pieData}
                          cx="50%"
                          cy="50%"
                          innerRadius={14}
                          outerRadius={26}
                          paddingAngle={2}
                          dataKey="value"
                          stroke="none"
                        >
                          {pieData.map((d, i) => (
                            <Cell key={i} fill={d.color} />
                          ))}
                        </Pie>
                      </PieChart>
                    </ResponsiveContainer>
                    <div className="flex flex-wrap justify-center gap-3 -mt-1">
                      {pieData.map((d, i) => (
                        <span key={i} className="flex items-center gap-1 text-xs text-gray-500">
                          <span className="w-2 h-2 rounded-full flex-shrink-0" style={{ backgroundColor: d.color }} />
                          {d.name} {fmtTokens(d.value, locale)}
                        </span>
                      ))}
                    </div>
                  </>
                );
              }
              // Fallback to category-level 2-slice pie
              return (
                <>
                  <ResponsiveContainer width="100%" height={60}>
                    <PieChart>
                      <Pie
                        data={[
                          { name: t('ts.tool'), value: totalCompoundedToolSaved },
                          { name: t('ts.mcp'), value: totalCompoundedMcpSaved },
                        ]}
                        cx="50%"
                        cy="50%"
                        innerRadius={14}
                        outerRadius={26}
                        paddingAngle={2}
                        dataKey="value"
                        stroke="none"
                      >
                        {SAVED_PIE_COLORS.map((c, i) => (
                          <Cell key={i} fill={c} />
                        ))}
                      </Pie>
                    </PieChart>
                  </ResponsiveContainer>
                  <div className="flex justify-center gap-4 -mt-1">
                    <span className="flex items-center gap-1 text-xs text-gray-500">
                      <span className="w-2 h-2 rounded-full bg-orange-500" />
                      {t('ts.tool')} {fmtTokens(totalCompoundedToolSaved, locale)}
                    </span>
                    <span className="flex items-center gap-1 text-xs text-gray-500">
                      <span className="w-2 h-2 rounded-full bg-violet-500" />
                      {t('ts.mcp')} {fmtTokens(totalCompoundedMcpSaved, locale)}
                    </span>
                  </div>
                </>
              );
            })()}
          </div>
        </div>

        {/* Card 3: Savings rate */}
        <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-5">
          <p className="text-sm text-gray-500">{t('ts.savingsRate')}</p>
          <div className="flex items-center gap-4 mt-1">
            <div className="relative w-20 h-20 flex-shrink-0">
              <svg viewBox="0 0 80 80" className="w-full h-full -rotate-90">
                <circle cx="40" cy="40" r="34" fill="none" stroke="#e5e7eb" strokeWidth="6" />
                <circle
                  cx="40"
                  cy="40"
                  r="34"
                  fill="none"
                  stroke={savingsRate >= 30 ? '#10b981' : savingsRate >= 15 ? '#3b82f6' : '#f59e0b'}
                  strokeWidth="6"
                  strokeDasharray={`${(Math.min(savingsRate, 100) / 100) * 213.6} 213.6`}
                  strokeLinecap="round"
                />
              </svg>
              <div className="absolute inset-0 flex items-center justify-center">
                <span className="text-lg font-bold text-gray-900">
                  {savingsRate.toFixed(1)}%
                </span>
              </div>
            </div>
            <div>
              <span
                className={`px-2 py-0.5 rounded text-xs font-medium ${
                  savingsRate >= 30
                    ? 'bg-green-100 text-green-700'
                    : savingsRate >= 15
                    ? 'bg-blue-100 text-blue-700'
                    : 'bg-orange-100 text-orange-700'
                }`}
              >
                {savingsRate >= 30 ? t('ts.excellent') : savingsRate >= 15 ? t('ts.good') : t('ts.needsWork')}
              </span>
              <p className="text-xs text-gray-400 mt-1">
                {t('ts.savingsRateFormula')}
              </p>
            </div>
          </div>
        </div>
      </div>

      {/* ── Baseline comparison bar ── */}
      {baselineTokens > 0 && totalCompoundedSaved > 0 && (() => {
        const usedPct = (totalTokens / baselineTokens) * 100;
        const savedPct = (totalCompoundedSaved / baselineTokens) * 100;
        return (
          <div className="bg-white rounded-xl shadow-sm border border-gray-200 px-5 py-4">
            <div className="flex items-baseline gap-2 mb-3">
              <span className="text-xs font-medium text-gray-400 uppercase tracking-wider">{t('ts.baselineConsumption')}</span>
              <span className="text-sm font-semibold text-gray-700">{fmtTokens(baselineTokens, locale)}</span>
            </div>
            <div className="flex h-3 rounded-full overflow-hidden bg-gray-100">
              <div className="bg-blue-500 transition-all" style={{ width: `${usedPct}%` }} />
              <div className="bg-emerald-400 transition-all" style={{ width: `${savedPct}%` }} />
            </div>
            <div className="flex items-center justify-between mt-2.5 text-xs text-gray-500">
              <span className="flex items-center gap-1.5">
                <span className="w-2 h-2 rounded-sm bg-blue-500" />
                {t('ts.actual')} {fmtTokens(totalTokens, locale)}
                <span className="text-gray-300">({usedPct.toFixed(1)}%)</span>
              </span>
              <span className="flex items-center gap-1.5 text-emerald-600 font-medium">
                <span className="w-2 h-2 rounded-sm bg-emerald-400" />
                {t('ts.saved')} {fmtTokens(totalCompoundedSaved, locale)}
                <span className="font-normal text-emerald-500">({savedPct.toFixed(1)}%)</span>
              </span>
            </div>
          </div>
        );
      })()}

      {/* ── Optimization tips + Savings breakdown ── */}
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <OptimizationTipsPanel tips={tips} />
        <SavingsBreakdownPanel sessions={sessions} />
      </div>

      {/* ── Session table ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full min-w-[800px]">
            <thead className="bg-gray-50 border-b border-gray-200">
              <tr>
                <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                  <span className="inline-flex items-center gap-1.5">
                    <span>{t('ts.sessionId')}</span>
                    <SessionIdHelp />
                  </span>
                </th>
                <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                  {t('common.agent')}
                </th>
                <th className="px-4 lg:px-6 py-3 text-right text-xs font-semibold text-gray-600 uppercase tracking-wide">
                  {t('ts.inputTokens')}
                </th>
                <th className="px-4 lg:px-6 py-3 text-right text-xs font-semibold text-gray-600 uppercase tracking-wide">
                  {t('ts.outputTokens')}
                </th>
                <th className="px-4 lg:px-6 py-3 text-right text-xs font-semibold text-gray-600 uppercase tracking-wide">
                  <span className="inline-flex items-center gap-1" title={t('ts.savedComparedTooltip')}>{t('ts.savedCol')}</span>
                </th>
                <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                  {t('ts.savingsRateCol')}
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {sessions.map((sess) => (
                <SessionRow
                  key={sess.session_id}
                  session={sess}
                  initialExpanded={sess.session_id === expandedSessionId}
                  rowRef={sess.session_id === expandedSessionId ? targetRowRef : undefined}
                />
              ))}
            </tbody>
          </table>
        </div>
      </div>

      </>
      ) : (
        /* Prompt before first query */
        <div className="flex flex-col items-center justify-center py-20 text-gray-400">
          <div className="text-5xl mb-4">⚡</div>
          <p className="text-base">{t('ts.selectTimeRange')}</p>
          <p className="text-xs mt-2">{t('ts.viewSavings')}</p>
        </div>
      )}
    </main>
  );
};
