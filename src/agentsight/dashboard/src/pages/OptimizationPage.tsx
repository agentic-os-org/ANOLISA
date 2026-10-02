import React, { Fragment, useCallback, useEffect, useState } from 'react';
import { useNavigate, useParams } from 'react-router-dom';
import { Cell, Pie, PieChart, ResponsiveContainer, Tooltip } from 'recharts';
import {
  fetchOptimizeHistory,
  fetchOptimizeResults,
  runOptimizeDimension,
  ApiRequestError,
} from '../utils/apiClient';
import type { OptimizeHistoryEntry } from '../utils/apiClient';
import { copyText } from '../components/CopyButton';
import type {
  AccIssue,
  AccuracyResult,
  AnalysisReport,
  CostStats,
  Failure,
  PerfReport,
  PerfStats,
  TrajectorySummary,
  WasteReport,
} from '../types/optimization';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';
import { fixLocusDiverges, fixLocusLabel } from '../utils/accuracyAttribution';
import TokenFlameChart from '../components/TokenFlameChart';

// ── 通用状态类型 ──────────────────────────────────────────────────────────────

type DimState = 'idle' | 'loading' | 'done' | 'error';
type AnalysisProgress = {
  summary: DimState;
  perf: DimState;
  perfIssues: DimState;
  cost: DimState;
  costWaste: DimState;
  accuracy: DimState;
};
type DimKey = keyof AnalysisProgress;
const ALL_DIMS: DimKey[] = ['summary', 'perf', 'perfIssues', 'cost', 'costWaste', 'accuracy'];
type AnalysisTab = 'accuracy' | 'perf' | 'cost';

const FAILURE_LABELS: Record<string, MessageKey> = {
  tool_error: 'opt.failure.toolErrorLabel',
  reasoning_error: 'opt.failure.reasoningErrorLabel',
  timeout: 'opt.failure.timeoutLabel',
  invalid_usage: 'opt.failure.invalidUsageLabel',
};

// ── 工具函数 ──────────────────────────────────────────────────────────────────

/** 从 ApiRequestError 中提取用户友好的错误信息，避免暴露完整 API URL */
function userFacingError(
  e: unknown,
  t: (key: MessageKey, params?: Record<string, string | number>) => string,
): string {
  if (e instanceof ApiRequestError) {
    const serverMsg = (e.body?.message ?? e.body?.error) as string | undefined;
    if (serverMsg) return serverMsg;
    return t('opt.error.requestFailedStatus', { status: e.status });
  }
  return e instanceof Error ? e.message : String(e);
}

const H = (s: string) => (
  <span
    className="[&_code]:bg-gray-100 [&_code]:px-1 [&_code]:py-0.5 [&_code]:rounded [&_code]:font-mono [&_code]:text-xs [&_code]:text-gray-800"
    dangerouslySetInnerHTML={{ __html: s }}
  />
);

function formatSecs(s: number): string {
  if (s >= 60) {
    const m = Math.floor(s / 60);
    const sec = Math.round(s % 60);
    return `${m}m ${sec}s`;
  }
  return `${s.toFixed(1)}s`;
}

function shortId(id: string, len = 20): string {
  return id.length > len ? id.slice(0, len) + '…' : id;
}

/** ns 时间戳 → 本地时间字符串 */
function fmtNs(ns: number, locale: string): string {
  return new Date(ns / 1e6).toLocaleString(locale);
}

const Spinner: React.FC<{ size?: number }> = ({ size = 18 }) => (
  <span
    className="inline-block rounded-full border-2 border-blue-500 border-t-transparent animate-spin flex-shrink-0"
    style={{ width: size, height: size }}
  />
);

// ── Section 头部 ──────────────────────────────────────────────────────────────

const SEC_TAG_CLS: Record<string, string> = {
  acc: 'bg-green-100 text-green-700',
  perf: 'bg-blue-100 text-blue-700',
  cost: 'bg-amber-100 text-amber-700',
};

const SectionHead: React.FC<{ idx: string; title: string; tag?: string; tagKind?: string }> = ({
  idx,
  title,
  tag,
  tagKind = 'acc',
}) => (
  <div className="flex items-center gap-2.5 mb-3">
    <span className="w-7 h-7 rounded-lg bg-gray-900 text-white flex items-center justify-center font-mono font-bold text-sm flex-shrink-0">
      {idx}
    </span>
    <h2 className="text-base font-semibold text-gray-900">{title}</h2>
    {tag && (
      <span className={`px-2 py-0.5 rounded text-xs font-medium ${SEC_TAG_CLS[tagKind] ?? SEC_TAG_CLS.acc}`}>
        {tag}
      </span>
    )}
  </div>
);

// ── 加载中 / 失败 / 未分析 占位 ───────────────────────────────────────────────

function LoadingBlock({ label }: { label: string }) {
  const { t } = useI18n();
  return (
    <section className="mt-6">
      <SectionHead idx="…" title={label} tag={t('opt.loading.analyzingTag')} />
      <div className="bg-white rounded-lg shadow border border-gray-200 flex items-center gap-3 px-6 py-5">
        <Spinner size={20} />
        <span className="text-gray-500 font-mono text-[13px]">
          {t('opt.loading.calculating')}
        </span>
      </div>
    </section>
  );
}

function ErrorBlock({ label }: { label: string }) {
  const { t } = useI18n();
  return (
    <section className="mt-6">
      <SectionHead idx="✗" title={label} />
      <div className="bg-red-50 border border-red-200 text-red-700 px-6 py-4 rounded-lg text-sm">
        {t('opt.error.analysisFailedHint')}
      </div>
    </section>
  );
}

function IdleBlock({ label }: { label: string }) {
  const { t } = useI18n();
  return (
    <section className="mt-6">
      <SectionHead idx="·" title={label} />
      <div className="bg-white rounded-lg shadow border border-gray-200 px-6 py-8 text-center text-gray-400 text-sm">
        {t('opt.dim.notAnalyzedHint')}
      </div>
    </section>
  );
}
// ── 轨迹摘要（LLM 叙事）──────────────────────────────────────────────────────

/**
 * 会话叙事摘要卡片：目标 / 过程 / 结果。
 *
 * 摘要完全依赖 LLM，所以 LLM 未配置、调用失败或返回空内容时整块不渲染
 * （不做非 LLM 兜底），避免在页面顶部留一块空壳。
 */
function SummaryCard({ state, summary }: { state: DimState; summary?: TrajectorySummary | null }) {
  const { t } = useI18n();

  if (state === 'loading') {
    return (
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 mt-4 px-6 py-5 flex items-center gap-3">
        <Spinner size={18} />
        <span className="text-gray-500 font-mono text-[13px]">
          {t('opt.summary.generating')}
        </span>
      </div>
    );
  }

  const hasContent =
    !!summary && (!!summary.goal?.trim() || summary.process?.length > 0 || !!summary.outcome?.trim());
  if (state !== 'done' || !hasContent) return null;

  return (
    <div className="bg-white rounded-xl shadow-sm border border-gray-200 mt-4 px-6 py-5">
      <h2 className="text-sm font-semibold text-gray-700 mb-3">{t('opt.summary.title')}</h2>

      {summary.goal?.trim() && (
        <div className="flex gap-3 text-sm">
          <span className="text-gray-400 flex-shrink-0 w-8">{t('opt.summary.goalLabel')}</span>
          <span className="text-gray-900">{summary.goal}</span>
        </div>
      )}

      {summary.process?.length > 0 && (
        <div className="flex gap-3 text-sm mt-2">
          <span className="text-gray-400 flex-shrink-0 w-8">{t('opt.summary.processLabel')}</span>
          <ol className="list-decimal list-inside space-y-1 text-gray-700 m-0">
            {summary.process.map((step, i) => (
              <li key={i}>{step}</li>
            ))}
          </ol>
        </div>
      )}

      {summary.outcome?.trim() && (
        <div className="flex gap-3 text-sm mt-2">
          <span className="text-gray-400 flex-shrink-0 w-8">{t('opt.summary.outcomeLabel')}</span>
          <span className="text-gray-900">{summary.outcome}</span>
        </div>
      )}
    </div>
  );
}

// ── 准确性维度：五字段归因表（移植自 agentopt Dimensions.tsx）────────────────

// 根因主因「在原地能修」时对应的修复落点定义在 utils/accuracyAttribution，
// 便于单测锁住「协议值不可本地化」这一不变式。

const CONF_CLS: Record<AccIssue['confidence'], string> = {
  '\u9ad8': 'bg-green-100 text-green-700',
  '\u4e2d': 'bg-yellow-100 text-yellow-700',
  '\u4f4e': 'bg-gray-100 text-gray-500',
};

const CONF_LABEL_KEY: Record<AccIssue['confidence'], MessageKey> = {
  '\u9ad8': 'common.high',
  '\u4e2d': 'common.medium',
  '\u4f4e': 'common.low',
};

// 构造可直接喂给 Agent 自我优化的优化提示词
function buildOptPrompt(it: AccIssue): string {
  const rc = it.rootCause.map((r) => r.object).join(', ');
  return `You are an agent skill optimization expert. Based on the diagnosis below, update the corresponding Skill definition to fix the problem.

## Diagnosis

- **Symptom**: ${it.symptom}
- **Defect type**: ${it.defectType}
- **Root cause object**: ${rc}
- **Fix locus**: ${it.fixLocus}
- **Location**: ${it.at}

## Evidence

${it.detail.replace(/<\/?code>/g, '`')}

## Optimization suggestions

${it.fix.replace(/<\/?code>/g, '`')}

Please output the updated Skill content (or a patch) and explain your changes.`;
}

// 复制按钮：复制后显示 ✓ 2秒
function CopyBtn({ text }: { text: string }) {
  const { t } = useI18n();
  const [done, setDone] = useState(false);
  const handleCopy = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      const ok = () => {
        setDone(true);
        setTimeout(() => setDone(false), 2000);
      };
      copyText(text, ok);
    },
    [text, t],
  );
  return (
    <button
      onClick={handleCopy}
      className={`px-2 py-1 rounded text-xs whitespace-nowrap transition-colors ${
        done
          ? 'bg-green-100 text-green-600'
          : 'bg-gray-100 hover:bg-gray-200 text-gray-500 hover:text-gray-700'
      }`}
      title={t('opt.copy.promptTitle')}
    >
      {done ? t('common.copied') : t('common.copy')}
    </button>
  );
}

function IssueTable({ issues }: { issues: AccIssue[] }) {
  const { t } = useI18n();
  const [open, setOpen] = useState<number | null>(0);
  return (
    <div className="overflow-x-auto">
      <table className="w-full min-w-[720px] text-sm">
        <thead>
          <tr className="border-b border-gray-200">
            <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600">
              {t('opt.accuracy.table.symptom')}
            </th>
            <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">
              {t('opt.accuracy.table.defectType')}
            </th>
            <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">
              {t('opt.accuracy.table.rootCause')}
            </th>
            <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">
              {t('opt.accuracy.table.fixLocus')}
            </th>
            <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">
              {t('opt.accuracy.table.confidence')}
            </th>
            <th className="text-left pb-2 text-xs font-semibold text-gray-600 whitespace-nowrap">
              {t('opt.accuracy.table.prompt')}
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-gray-100">
          {issues.map((it, i) => {
            const isOpen = open === i;
            const primary = it.rootCause.find((rc) => rc.role === '\u4e3b\u56e0')?.object ?? '';
            const fixDiverges = fixLocusDiverges(primary, it.fixLocus);
            const fixLocusText = fixLocusLabel(it.fixLocus, t);
            const fixTitle = fixDiverges
              ? t('opt.accuracy.fixDiverges', { primary, fixLocus: fixLocusText })
              : t('opt.accuracy.fixSame');
            const confCls = CONF_CLS[it.confidence] ?? 'bg-gray-100 text-gray-500';
            const confKey = CONF_LABEL_KEY[it.confidence];
            const confLabel = confKey ? t(confKey) : it.confidence;

            return (
              <Fragment key={i}>
                <tr
                  className={`cursor-pointer transition-colors ${
                    isOpen ? 'bg-blue-50' : 'hover:bg-gray-50'
                  } ${it.optimizable ? '' : 'opacity-60'}`}
                  onClick={() => setOpen(isOpen ? null : i)}
                >
                  <td className="py-2.5 pr-3 text-gray-800">
                    <span className="text-gray-400 text-xs mr-1.5">{isOpen ? '▼' : '▶'}</span>
                    {it.symptom}
                    {it.recovered && (
                      <span className="ml-1.5 px-1.5 py-0.5 rounded bg-amber-100 text-amber-700 text-[10px] whitespace-nowrap">
                        {t('opt.accuracy.recoveredBadge')}
                      </span>
                    )}
                  </td>
                  <td className="py-2.5 pr-3 whitespace-nowrap">
                    <span className="px-1.5 py-0.5 rounded bg-gray-100 text-gray-700 font-mono text-xs">
                      {it.defectType}
                    </span>
                  </td>
                  <td className="py-2.5 pr-3 whitespace-nowrap">
                    {it.rootCause.map((rc) => (
                      <span
                        className="px-1.5 py-0.5 rounded bg-blue-100 text-blue-700 text-xs mr-1"
                        key={rc.object}
                      >
                        {rc.object}
                      </span>
                    ))}
                  </td>
                  <td className="py-2.5 pr-3 whitespace-nowrap">
                    <span
                      className={`text-xs font-medium ${
                        fixDiverges ? 'text-amber-600' : 'text-gray-600'
                      }`}
                      title={fixTitle}
                    >
                      → {fixLocusText}
                    </span>
                  </td>
                  <td className="py-2.5 pr-3 whitespace-nowrap">
                    <span className={`px-1.5 py-0.5 rounded text-xs font-medium ${confCls}`}>
                      {confLabel}
                    </span>
                  </td>
                  <td className="py-2.5 whitespace-nowrap">
                    {it.optimizable ? (
                      <CopyBtn text={buildOptPrompt(it)} />
                    ) : (
                      <span className="px-2 py-1 rounded bg-gray-100 text-gray-400 text-xs">
                        {t('opt.accuracy.notOptimizable')}
                      </span>
                    )}
                  </td>
                </tr>
                {isOpen && (
                  <tr className="bg-gray-50">
                    <td colSpan={6} className="px-4 py-3">
                      <dl className="text-sm space-y-2">
                        <div>
                          <dt className="text-xs font-semibold text-gray-500 uppercase tracking-wide">
                            {t('opt.accuracy.detail.evidence')}
                          </dt>
                          <dd className="mt-1 text-gray-700">
                            {H(it.detail)}{' '}
                            <span className="font-mono text-xs text-gray-400">{it.at}</span>
                          </dd>
                        </div>
                        <div>
                          <dt className="text-xs font-semibold text-gray-500 uppercase tracking-wide">
                            {t('opt.accuracy.detail.verify')}
                          </dt>
                          <dd className="mt-1 text-gray-700">{it.verify}</dd>
                        </div>
                        <div>
                          <dt className="text-xs font-semibold text-gray-500 uppercase tracking-wide">
                            {t('opt.accuracy.detail.fix')}
                          </dt>
                          <dd className="mt-1 text-gray-700">{H(it.fix)}</dd>
                        </div>
                      </dl>
                    </td>
                  </tr>
                )}
              </Fragment>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

// ── 准确性维度 Section ────────────────────────────────────────────────────────

function FailureRow({ f, index }: { f: Failure; index: number }) {
  const { t } = useI18n();
  const labelKey = FAILURE_LABELS[f.failure_type];
  const label = labelKey ? t(labelKey) : f.failure_type;
  return (
    <div className="flex items-start gap-3 mb-3">
      <span
        className={`px-2 py-0.5 rounded text-xs font-medium flex-shrink-0 ${
          f.recovery ? 'bg-yellow-100 text-yellow-700' : 'bg-red-100 text-red-700'
        }`}
      >
        {label}
      </span>
      <div className="text-sm">
        <p className="font-semibold text-gray-800">#{index + 1} {f.description}</p>
        <p className="mt-1.5 text-gray-600">{f.context}</p>
        {f.recovery && (
          <p className="mt-1.5 text-green-600">
            {t('opt.accuracy.failureRecovery', { text: f.recovery })}
          </p>
        )}
      </div>
    </div>
  );
}

function AccuracySection({ issues, failures }: { issues: AccIssue[]; failures: Failure[] }) {
  const { t } = useI18n();
  const count = issues.length > 0 ? issues.length : failures.length;
  const tag = t('opt.accuracy.issueCountTag', { count });
  return (
    <section className="mt-6">
      <SectionHead idx="A" title={t('opt.accuracy.sectionTitle')} tag={tag} tagKind="acc" />
      {count === 0 ? (
        <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
          <p className="text-green-600 font-mono text-sm">{t('opt.accuracy.noIssues')}</p>
        </div>
      ) : issues.length > 0 ? (
        // 五字段正交归因表（现象 / 缺陷类型 / 归因对象 / 修复落点 / 置信度 / 可优化）
        <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
          <h3 className="text-sm font-semibold text-gray-700 mb-3">
            {t('opt.accuracy.issueTableTitle')}
          </h3>
          <IssueTable issues={issues} />
        </div>
      ) : (
        // 兼容旧会话：无五字段归因时回退到旧失败清单
        <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
          <h3 className="text-sm font-semibold text-gray-700 mb-3">
            {t('opt.accuracy.issueTableTitleFallback')}
          </h3>
          {failures.map((f, i) => (
            <FailureRow key={i} f={f} index={i} />
          ))}
        </div>
      )}
    </section>
  );
}

// ── 性能维度 ──────────────────────────────────────────────────────────────────

// 大类配色（agentsight 主题：推理=蓝 / 工具=绿 / 用户空闲=橙）
const PERF_CAT_COLOR: Record<string, string> = {
  '\u6a21\u578b\u63a8\u7406\u6162': '#3b82f6',
  '\u5de5\u5177\u6267\u884c\u6162': '#10b981',
  '\u7528\u6237\u7a7a\u95f2': '#f59e0b',
};

function PerfIssueTable({ issues, state }: { issues: PerfReport | null; state: DimState }) {
  const { t } = useI18n();
  // 加载中 / 失败 / 未分析态
  if (state !== 'done' || !issues) {
    return (
      <div className="bg-white rounded-lg shadow border border-gray-200 p-5 mt-4">
        {state === 'error' ? (
          <p className="text-red-500 font-mono text-[13px]">{t('opt.perf.state.error')}</p>
        ) : state === 'loading' ? (
          <div className="flex items-center gap-3 py-2">
            <Spinner />
            <span className="text-gray-500 font-mono text-[13px]">{t('opt.perf.state.loading')}</span>
          </div>
        ) : (
          <p className="text-gray-400 font-mono text-[13px]">{t('opt.perf.state.idle')}</p>
        )}
      </div>
    );
  }

  if (issues.items.length === 0) {
    return (
      <div className="bg-white rounded-lg shadow border border-gray-200 p-5 mt-4">
        <p className="text-green-600 font-mono text-[13px]">
          {t('opt.perf.state.noStrategies')}
        </p>
      </div>
    );
  }

  return (
    <div className="bg-white rounded-lg shadow border border-gray-200 p-5 mt-4">
      <div className="overflow-x-auto">
        <table className="w-full min-w-[700px] text-sm">
          <thead>
            <tr className="border-b border-gray-200">
              <th className="text-left pb-2 pr-4 text-xs font-semibold text-gray-600 w-[30%]">
                {t('opt.perf.table.symptom')}
              </th>
              <th className="text-left pb-2 pr-4 text-xs font-semibold text-gray-600 whitespace-nowrap">
                {t('opt.perf.table.strategyType')}
              </th>
              <th className="text-left pb-2 pr-4 text-xs font-semibold text-gray-600 w-[26%]">
                {t('opt.perf.table.rootCause')}
              </th>
              <th className="text-left pb-2 text-xs font-semibold text-gray-600 w-[28%]">
                {t('opt.perf.table.optimization')}
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-100">
            {issues.items.map((it, i) => (
              <tr key={i} className="align-top">
                <td className="py-3 pr-4 text-[13px] text-gray-800 leading-relaxed">
                  {it.symptom}
                  {it.at && <span className="text-[10px] text-gray-400 font-mono"> · {it.at}</span>}
                </td>
                <td className="py-3 pr-4">
                  <span className="inline-flex items-center gap-1.5 whitespace-nowrap">
                    <i
                      className="w-[9px] h-[9px] rounded-sm flex-shrink-0"
                      style={{ background: PERF_CAT_COLOR[it.category] ?? '#3b82f6' }}
                    />
                    <span className="font-mono text-xs text-gray-700">{it.subtype}</span>
                  </span>
                </td>
                <td className="py-3 pr-4 text-xs text-gray-500 leading-relaxed">{it.root_cause || it.evidence}</td>
                <td className="py-3 text-xs text-gray-700 leading-relaxed">{it.optimization}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function PerfSection({ perf, issues, issuesState }: { perf: PerfStats; issues: PerfReport | null; issuesState: DimState }) {
  const { t } = useI18n();
  const wall = perf.wall_secs || 1;
  const modelPct = Math.round((perf.model_secs / wall) * 100);
  const toolPct = Math.round((perf.tool_secs / wall) * 100);
  const idlePct = Math.max(0, 100 - modelPct - toolPct);
  // 环形图三分：仅保留有实际占用的分类，避免 0 值扇区
  const timeSlices = [
    { name: t('opt.perf.slice.model'), secs: perf.model_secs, pct: modelPct, color: '#3b82f6' },
    { name: t('opt.perf.slice.tools'), secs: perf.tool_secs, pct: toolPct, color: '#10b981' },
    { name: t('opt.perf.slice.idle'), secs: perf.idle_secs, pct: idlePct, color: '#9ca3af' },
  ].filter((s) => s.secs > 1);

  return (
    <section className="mt-6">
      <SectionHead idx="P" title={t('opt.perf.sectionTitle')} tag={formatSecs(perf.wall_secs)} tagKind="perf" />

      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <div className="bg-white rounded-lg shadow border border-gray-200 p-5 flex flex-col">
          <h3 className="text-sm font-semibold text-gray-700 mb-3">{t('opt.perf.timeDistributionTitle')}</h3>
          {/* 图 + 图例整体居中（卡片比右侧表格短，同时垂直居中）*/}
          <div className="flex flex-1 items-center justify-center gap-6">
            <div className="relative w-[150px] h-[150px] flex-shrink-0">
              <ResponsiveContainer width="100%" height="100%">
                <PieChart>
                  <Pie
                    data={timeSlices}
                    cx="50%"
                    cy="50%"
                    innerRadius={48}
                    outerRadius={72}
                    paddingAngle={2}
                    dataKey="secs"
                    stroke="none"
                  >
                    {timeSlices.map((s) => (
                      <Cell key={s.name} fill={s.color} />
                    ))}
                  </Pie>
                  <Tooltip formatter={(v: number) => formatSecs(v)} />
                </PieChart>
              </ResponsiveContainer>
              {/* 环心承载总量，替代原先卡片底部的汇总文字 */}
              <div className="absolute inset-0 flex flex-col items-center justify-center pointer-events-none">
                <span className="text-sm font-semibold text-gray-900">{formatSecs(perf.wall_secs)}</span>
                <span className="text-[10px] text-gray-400 mt-0.5">
                  {t('opt.perf.totalToolCalls', { n: perf.tool_count })}
                </span>
              </div>
            </div>
            <div className="flex flex-col gap-2.5 min-w-0">
              {timeSlices.map((s) => (
                <div key={s.name} className="flex items-center gap-2 text-xs">
                  <i className="w-2.5 h-2.5 rounded-sm flex-shrink-0" style={{ background: s.color }} />
                  <span className="text-gray-600">{s.name}</span>
                  <span className="text-gray-900 font-medium">{formatSecs(s.secs)}</span>
                  <span className="text-gray-400 font-mono">{s.pct}%</span>
                </div>
              ))}
            </div>
          </div>
        </div>

        <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
          <h3 className="text-sm font-semibold text-gray-700 mb-3">{t('opt.perf.slowestCallsTitle')}</h3>
          <table className="w-full table-fixed text-sm">
            <thead>
              <tr className="border-b border-gray-200">
                <th className="w-[110px] text-left pb-2 pr-3 text-xs font-semibold text-gray-600">
                  {t('opt.perf.table.toolName')}
                </th>
                <th className="w-[70px] text-left pb-2 pr-3 text-xs font-semibold text-gray-600">
                  {t('opt.perf.table.duration')}
                </th>
                <th className="text-left pb-2 text-xs font-semibold text-gray-600">
                  {t('opt.perf.table.command')}
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-100">
              {perf.top_slow.length === 0 ? (
                <tr>
                  <td colSpan={3} className="py-6 text-center text-gray-400 text-xs">
                    {t('opt.perf.noToolCalls')}
                  </td>
                </tr>
              ) : (
                perf.top_slow.slice(0, 5).map((call, i) => (
                  <tr key={i}>
                    <td
                      title={call.name}
                      className="py-2 pr-3 whitespace-nowrap overflow-hidden text-ellipsis text-gray-800"
                    >
                      {call.name}
                    </td>
                    <td className={`py-2 pr-3 whitespace-nowrap ${call.err ? 'text-red-500' : 'text-gray-600'}`}>
                      {formatSecs(call.dur)}{call.err ? ' ✗' : ''}
                    </td>
                    <td
                      title={call.cmd}
                      className="py-2 text-[11px] font-mono text-gray-500 overflow-hidden text-ellipsis whitespace-nowrap"
                    >
                      {call.cmd}
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>
      </div>

      {/* 优化策略表（Rust 供数 → LLM 选择适用策略 + 根因/建议）*/}
      <PerfIssueTable issues={issues} state={issuesState} />
    </section>
  );
}

// ── 成本维度 ──────────────────────────────────────────────────────────────────

function CostSection({ cost, waste, wasteState }: { cost: CostStats; waste: WasteReport | null; wasteState: DimState }) {
  const { t } = useI18n();
  const h = cost.headroom;
  const fmtK = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : `${Math.round(n)}`);

  // findings 只渲染数据质量警告（采集退化）。旧落库 payload 里还带着已删除的
  // 启发式观察条（工具返回占比/重复调用/思考占比，与火焰图和浪费诊断表重复），
  // 按内容过滤掉，不必等重新分析。
  const qualityFindings = cost.findings.filter((f) => f.html.includes('\u91c7\u96c6\u4e0d\u5b8c\u6574'));

  const hrSavePct = h?.headroom_save_pct ?? 0;
  const isReal = hrSavePct > 0;
  const usageSteps = cost.usage_steps ?? 0;
  const totalSteps = cost.calls?.length ?? 0;
  const tokSource =
    usageSteps > 0
      ? usageSteps === totalSteps
        ? t('opt.cost.tokSourceMeasuredUsage')
        : t('opt.cost.tokSourceMeasuredSteps', { used: usageSteps, total: totalSteps })
      : t('opt.cost.tokSourceEstimated');

  const totalTokens = h ? h.total_input_tok + h.total_output_tok : 0;

  return (
    <section className="mt-6">
      <SectionHead
        idx="$"
        title={t('opt.cost.sectionTitle')}
        tagKind="cost"
        tag={
          h
            ? isReal
              ? t('opt.cost.tag.headroomMeasured', {
                  tokens: fmtK(totalTokens),
                  pct: hrSavePct.toFixed(0),
                })
              : t('opt.cost.tag.headroom', { tokens: fmtK(totalTokens) })
            : t('opt.cost.tag.noHeadroom', {
                events: cost.total_events,
                chars: cost.total_chars.toLocaleString(),
              })
        }
      />

      {/* 元数据脚注（原「内容体积」卡降级：字符/事件/步数/token 来源属数据质量标注，不占卡位；
          「可优化 N%」估算徽标与 Headroom 卡同源，一并移除，实测分支保留） */}
      {h && totalTokens > 0 && (
        <div className="text-[11px] text-gray-500 font-mono mb-3">
          {t('opt.cost.meta', {
            chars: cost.total_chars.toLocaleString(),
            events: cost.total_events,
            calls: cost.calls?.length ?? 0,
            source: tokSource,
          })}
        </div>
      )}

      {/* 数据质量警告（采集退化）— 必须醒目，避免退化轨迹被误读为干净 */}
      {qualityFindings.length > 0 && (
        <div className="space-y-2 mb-4">
          {qualityFindings.map((f, i) => (
            <div
              key={i}
              className={`text-[12px] leading-relaxed rounded-md border px-3 py-2 ${
                f.severity === 'high'
                  ? 'bg-rose-50 border-rose-200 text-rose-800'
                  : 'bg-amber-50 border-amber-200 text-amber-800'
              }`}
            >
              {H(f.html)}
            </div>
          ))}
        </div>
      )}

      {/* Token 火焰图：逐步 context window 重放拆解 + LLM 浪费诊断 */}
      <TokenFlameChart cost={cost} waste={waste} wasteState={wasteState} />
    </section>
  );
}

// ── 三 TAB 渐进式分析视图（移植自 agentopt AnalysisView.tsx）─────────────────

const TAB_DEFS: { key: AnalysisTab; nameKey: MessageKey; labelKey: MessageKey }[] = [
  { key: 'accuracy', nameKey: 'opt.tab.accuracy.name', labelKey: 'opt.tab.accuracy.label' },
  { key: 'perf', nameKey: 'opt.tab.perf.name', labelKey: 'opt.tab.perf.label' },
  { key: 'cost', nameKey: 'opt.tab.cost.name', labelKey: 'opt.tab.cost.label' },
];

const TAB_DOT: Record<AnalysisTab, string> = {
  accuracy: 'bg-green-500',
  perf: 'bg-blue-500',
  cost: 'bg-amber-500',
};

function AnalysisView({ report, progress }: { report: AnalysisReport; progress: AnalysisProgress }) {
  const { t } = useI18n();
  const [tab, setTab] = useState<AnalysisTab>('perf');

  // Composite state per tab: perf/cost each have 2 phases (stats + LLM)
  function tabState(tk: AnalysisTab): {
    indicator: 'idle' | 'loading' | 'partial' | 'done' | 'error';
    doneCount: number;
    totalCount: number;
  } {
    if (tk === 'accuracy') {
      const s = progress.accuracy;
      return { indicator: s, doneCount: s === 'done' ? 1 : 0, totalCount: 1 };
    }
    const s1 = tk === 'perf' ? progress.perf : progress.cost;
    const s2 = tk === 'perf' ? progress.perfIssues : progress.costWaste;
    if (s1 === 'idle') return { indicator: 'idle', doneCount: 0, totalCount: 2 };
    if (s1 === 'error') return { indicator: 'error', doneCount: 0, totalCount: 2 };
    if (s1 === 'loading') return { indicator: 'loading', doneCount: 0, totalCount: 2 };
    // s1 done
    if (s2 === 'done') return { indicator: 'done', doneCount: 2, totalCount: 2 };
    if (s2 === 'error' || s2 === 'idle') return { indicator: 'done', doneCount: 2, totalCount: 2 }; // partial error/idle still shows content
    return { indicator: 'partial', doneCount: 1, totalCount: 2 };
  }

  const state = tabState(tab);
  const currentDef = TAB_DEFS.find((d) => d.key === tab)!;
  // Content is viewable once the base stats are done (even if LLM is still running)
  const contentReady =
    tab === 'accuracy'
      ? progress.accuracy === 'done'
      : tab === 'perf'
        ? progress.perf === 'done' && !!report.perf
        : progress.cost === 'done' && !!report.cost;

  return (
    <>
      {/* 维度切换 Tab */}
      <div
        className="flex gap-1 bg-white rounded-lg shadow border border-gray-200 p-1 w-fit"
        role="tablist"
        aria-label={t('opt.tabs.ariaLabel')}
      >
        {TAB_DEFS.map((tdef) => {
          const ts = tabState(tdef.key);
          const active = tab === tdef.key;
          return (
            <button
              key={tdef.key}
              role="tab"
              aria-selected={active}
              className={`flex items-center px-4 py-2 rounded-md text-sm font-medium transition-colors ${
                active ? 'bg-blue-100 text-blue-700' : 'text-gray-600 hover:bg-gray-100'
              }`}
              onClick={() => setTab(tdef.key)}
            >
              <span className={`w-2 h-2 rounded-full mr-2 ${TAB_DOT[tdef.key]}`} />
              {t(tdef.nameKey)}
              {ts.indicator === 'loading' && (
                <span className="ml-1.5"><Spinner size={14} /></span>
              )}
              {ts.indicator === 'partial' && (
                <span className="ml-1.5 text-[11px] text-gray-500 font-mono inline-flex items-center gap-1">
                  {ts.doneCount}/{ts.totalCount}
                  <Spinner size={11} />
                </span>
              )}
              {ts.indicator === 'done' && <span className="ml-1.5 text-green-500 leading-none">✓</span>}
              {ts.indicator === 'error' && <span className="ml-1.5 text-red-500 leading-none">✗</span>}
            </button>
          );
        })}
      </div>

      {/* 当前 tab 内容 */}
      {!contentReady && state.indicator === 'error' && <ErrorBlock label={t(currentDef.labelKey)} />}
      {!contentReady && state.indicator === 'idle' && <IdleBlock label={t(currentDef.labelKey)} />}
      {!contentReady && (state.indicator === 'loading' || state.indicator === 'partial') && (
        <LoadingBlock label={t(currentDef.labelKey)} />
      )}
      {contentReady && tab === 'accuracy' && (
        <AccuracySection issues={report.issues ?? []} failures={report.failures} />
      )}
      {contentReady && tab === 'perf' && report.perf && (
        <PerfSection perf={report.perf} issues={report.perf_issues ?? null} issuesState={progress.perfIssues} />
      )}
      {contentReady && tab === 'cost' && report.cost && (
        <CostSection cost={report.cost} waste={report.cost_waste ?? null} wasteState={progress.costWaste} />
      )}
    </>
  );
}

// ── 会话分析视图 ──────────────────────────────────────────────────────────────

const EMPTY_REPORT: AnalysisReport = {
  extraction: { final_answer: '' },
  failures: [],
  issues: [],
  perf: null,
  perf_issues: null,
  cost: null,
  cost_waste: null,
  summary: null,
};

const IDLE_PROGRESS: AnalysisProgress = {
  summary: 'idle',
  perf: 'idle',
  perfIssues: 'idle',
  cost: 'idle',
  costWaste: 'idle',
  accuracy: 'idle',
};

function SessionAnalysisView({ sessionId }: { sessionId: string }) {
  const { t } = useI18n();
  const navigate = useNavigate();
  const [report, setReport] = useState<AnalysisReport>(EMPTY_REPORT);
  const [progress, setProgress] = useState<AnalysisProgress>(IDLE_PROGRESS);
  const [loadingResults, setLoadingResults] = useState(true);
  const [llmNotConfigured, setLlmNotConfigured] = useState(false);
  const [analyzeError, setAnalyzeError] = useState<string | null>(null);

  // 进入分析页时先加载历史结果展示
  useEffect(() => {
    let cancelled = false;
    setLoadingResults(true);
    setReport(EMPTY_REPORT);
    setProgress(IDLE_PROGRESS);
    setAnalyzeError(null);
    setLlmNotConfigured(false);
    (async () => {
      try {
        const data = await fetchOptimizeResults(sessionId);
        if (cancelled) return;
        setReport({
          extraction: data.accuracy?.extraction ?? { final_answer: '' },
          failures: data.accuracy?.failures ?? [],
          issues: data.accuracy?.issues ?? [],
          perf: data.perf,
          perf_issues: data.perf_issues,
          cost: data.cost,
          cost_waste: data.cost_waste,
          summary: data.summary,
        });
        setProgress({
          summary: data.summary ? 'done' : 'idle',
          perf: data.perf ? 'done' : 'idle',
          perfIssues: data.perf_issues ? 'done' : 'idle',
          cost: data.cost ? 'done' : 'idle',
          costWaste: data.cost_waste ? 'done' : 'idle',
          accuracy: data.accuracy ? 'done' : 'idle',
        });
      } catch {
        // 无历史结果时保持 idle，不阻塞页面
      } finally {
        if (!cancelled) setLoadingResults(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [sessionId]);

  // 维度请求失败的统一处理：400 llm_not_configured 时提示去设置里配置 LLM
  const handleDimError = useCallback((e: unknown) => {
    if (e instanceof ApiRequestError && e.status === 400 && e.body?.error === 'llm_not_configured') {
      setLlmNotConfigured(true);
    }
  }, []);

  // 渐进式分析：按维度触发，每个维度独立更新 loading/done/error
  const runDimensions = useCallback(
    (dims: DimKey[]) => {
      const has = (d: DimKey) => dims.includes(d);
      setProgress((prev) => {
        const next = { ...prev };
        dims.forEach((d) => {
          next[d] = 'loading';
        });
        return next;
      });

      // summary — 叙事摘要，单次 LLM 调用，数秒
      if (has('summary'))
        runOptimizeDimension<TrajectorySummary>(sessionId, 'summary')
          .then((data) => {
            setReport((prev) => ({ ...prev, summary: data }));
            setProgress((prev) => ({ ...prev, summary: 'done' }));
          })
          .catch((e) => {
            handleDimError(e);
            setProgress((prev) => ({ ...prev, summary: 'error' }));
          });

      // perf — 纯计算，毫秒级
      if (has('perf'))
        runOptimizeDimension<PerfStats>(sessionId, 'perf')
          .then((data) => {
            setReport((prev) => ({ ...prev, perf: data }));
            setProgress((prev) => ({ ...prev, perf: 'done' }));
          })
          .catch((e) => {
            handleDimError(e);
            setProgress((prev) => ({ ...prev, perf: 'error' }));
          });

      // perf-issues — Rust 供数 + LLM 策略选择，10-30s
      if (has('perfIssues'))
        runOptimizeDimension<PerfReport>(sessionId, 'perf-issues')
          .then((data) => {
            setReport((prev) => ({ ...prev, perf_issues: data }));
            setProgress((prev) => ({ ...prev, perfIssues: 'done' }));
          })
          .catch((e) => {
            handleDimError(e);
            setProgress((prev) => ({ ...prev, perfIssues: 'error' }));
          });

      // cost — 纯计算，毫秒级
      if (has('cost'))
        runOptimizeDimension<CostStats>(sessionId, 'cost')
          .then((data) => {
            setReport((prev) => ({ ...prev, cost: data }));
            setProgress((prev) => ({ ...prev, cost: 'done' }));
          })
          .catch((e) => {
            handleDimError(e);
            setProgress((prev) => ({ ...prev, cost: 'error' }));
          });

      // cost-waste — Rust 候选 + LLM 判定，10-30s
      if (has('costWaste'))
        runOptimizeDimension<WasteReport>(sessionId, 'cost-waste')
          .then((data) => {
            setReport((prev) => ({ ...prev, cost_waste: data }));
            setProgress((prev) => ({ ...prev, costWaste: 'done' }));
          })
          .catch((e) => {
            handleDimError(e);
            setProgress((prev) => ({ ...prev, costWaste: 'error' }));
          });

      // accuracy — LLM 多检测器，30-60s+，不设短超时
      if (has('accuracy'))
        runOptimizeDimension<AccuracyResult>(sessionId, 'accuracy')
          .then((data) => {
            setReport((prev) => ({
              ...prev,
              extraction: data.extraction,
              failures: data.failures,
              issues: data.issues ?? [],
            }));
            setProgress((prev) => ({ ...prev, accuracy: 'done' }));
          })
          .catch((e) => {
            handleDimError(e);
            setProgress((prev) => ({ ...prev, accuracy: 'error' }));
            setAnalyzeError(t('opt.accuracy.analyzeFailed', { msg: userFacingError(e, t) }));
          });
    },
    [sessionId, handleDimError, t],
  );

  // 全量重新分析（「重新分析」按钮）：清空已有结果后并行触发全部维度
  const runAnalysis = useCallback(() => {
    setReport(EMPTY_REPORT);
    setAnalyzeError(null);
    setLlmNotConfigured(false);
    runDimensions(ALL_DIMS);
  }, [runDimensions]);

  // 历史结果加载后自动补齐缺失维度：全空 = 首次进入，自动全量分析；
  // 部分缺失（如 cost 已落库但 cost-waste 从未跑成）只补跑缺的维度，
  // 避免这些维度停在「尚未分析」死等手动重跑
  useEffect(() => {
    if (loadingResults) return;
    const idle = ALL_DIMS.filter((k) => progress[k] === 'idle');
    if (idle.length === 0) return;
    if (idle.length === ALL_DIMS.length) {
      runAnalysis();
    } else {
      runDimensions(idle);
    }
  }, [loadingResults, progress, runAnalysis, runDimensions]);

  const running = Object.values(progress).some((s) => s === 'loading');
  const hasAnyResult = Object.values(progress).some((s) => s === 'done');

  return (
    <main className="max-w-screen-xl mx-auto px-6 py-6">
      {/* ── Header bar ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-center gap-3">
        <button
          onClick={() => navigate('/optimization')}
          className="px-3 py-1.5 text-sm bg-gray-100 hover:bg-gray-200 rounded-lg text-gray-600 transition-colors"
        >
          {t('opt.session.backToList')}
        </button>
        <div className="min-w-0">
          <p className="text-xs text-gray-400">{t('opt.session.headerTitle')}</p>
          <p className="font-mono text-sm text-gray-800 truncate" title={sessionId}>{sessionId}</p>
          <div className="flex items-center gap-3 mt-1">
            {/* 轨迹在新标签页打开：分析页可能正在跑维度（LLM 调用 10–60s），
                同标签跳走会卸载组件、丢掉进行中的分析 */}
            <a
              href={`#/atif?type=session&id=${encodeURIComponent(sessionId)}`}
              target="_blank"
              rel="noopener noreferrer"
              className="text-xs text-blue-600 hover:text-blue-800 hover:underline"
            >
              {t('opt.session.viewSourceTrajectory')}
            </a>
            <a
              href={`#/atif?type=session&id=${encodeURIComponent(`opt:${sessionId}`)}`}
              target="_blank"
              rel="noopener noreferrer"
              className="text-xs text-blue-600 hover:text-blue-800 hover:underline"
            >
              {t('opt.session.viewAnalysisTrajectory')}
            </a>
          </div>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <button
            onClick={runAnalysis}
            disabled={running || loadingResults}
            className="px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 transition-colors disabled:opacity-50"
          >
            {running ? t('opt.session.action.analyzing') : hasAnyResult ? t('opt.session.action.reanalyze') : t('opt.session.action.start')}
          </button>
        </div>
      </div>

      {/* ── 轨迹摘要（LLM 未配置 / 失败 / 空内容时整块不渲染）── */}
      {!llmNotConfigured && <SummaryCard state={progress.summary} summary={report.summary} />}

      {/* ── LLM 未配置提示 ── */}
      {llmNotConfigured && (
        <div className="mt-4 bg-yellow-50 border border-yellow-200 text-yellow-800 px-4 py-3 rounded-lg text-sm flex items-center gap-2 flex-wrap">
          <span>{t('opt.llm.notConfiguredHint')}</span>
          <button
            onClick={() => navigate('/settings')}
            className="underline font-medium hover:text-yellow-900"
          >
            {t('opt.llm.goToSettings')}
          </button>
        </div>
      )}

      {/* ── 准确性错误 ── */}
      {analyzeError && !llmNotConfigured && (
        <div className="mt-4 bg-red-50 border border-red-200 text-red-700 px-4 py-3 rounded-lg text-sm break-all">
          {analyzeError}
        </div>
      )}

      {/* ── 摘要行 ── */}
      <div className="mt-4 mb-4 text-sm text-gray-500">
        {progress.accuracy === 'done'
          ? t('opt.summaryRow.issueCount', {
              count: (report.issues?.length ?? 0) || report.failures.length,
            })
          : progress.accuracy === 'loading'
            ? t('opt.summaryRow.accuracyLoading')
            : ''}
        {report.perf &&
          t('opt.summaryRow.perf', {
            toolCount: report.perf.tool_count,
            seconds: Math.round(report.perf.wall_secs),
          })}
        {report.cost &&
          t('opt.summaryRow.cost', {
            events: report.cost.total_events,
          })}
      </div>

      {/* ── 内容 ── */}
      {loadingResults ? (
        <div className="flex items-center gap-3 py-16 justify-center text-gray-400">
          <Spinner size={20} />
          <span className="text-sm">{t('opt.session.loadingHistory')}</span>
        </div>
      ) : !hasAnyResult && !running ? (
        <div className="flex flex-col items-center justify-center py-20 text-gray-400">
          <div className="text-5xl mb-4">🔬</div>
          <p className="text-base">{t('opt.session.noAnalysisYet')}</p>
          <p className="text-xs mt-2">{t('opt.session.startHint')}</p>
        </div>
      ) : (
        <AnalysisView report={report} progress={progress} />
      )}
    </main>
  );
}
/** 维度键 → 标签 key（与分析页各 section 的叫法保持一致）*/
const DIM_LABELS: Record<string, MessageKey> = {
  summary: 'opt.dim.summary',
  perf: 'opt.dim.perf',
  perf_issues: 'opt.dim.perfStrategy',
  cost: 'opt.dim.cost',
  cost_waste: 'opt.dim.costWaste',
  accuracy: 'opt.dim.accuracy',
};

/** 维度标签配色：复用 SEC_TAG_CLS 的三色语义（准确性绿 / 性能蓝 / 成本橙），摘要用中性灰 */
const DIM_TAG_CLS: Record<string, string> = {
  summary: 'bg-gray-100 text-gray-600',
  perf: SEC_TAG_CLS.perf,
  perf_issues: SEC_TAG_CLS.perf,
  cost: SEC_TAG_CLS.cost,
  cost_waste: SEC_TAG_CLS.cost,
  accuracy: SEC_TAG_CLS.acc,
};

/** 历史记录每页行数（与 AgentSessionsPage 保持一致）。 */
const HISTORY_PAGE_SIZE = 15;
/** 一次拉取的最大条数——服务端 `/api/optimize/results` 的硬上限。 */
const HISTORY_FETCH_LIMIT = 200;

function SessionEntryView() {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const navigate = useNavigate();
  const [input, setInput] = useState('');
  const [history, setHistory] = useState<OptimizeHistoryEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [page, setPage] = useState(1);
  const trimmed = input.trim();

  useEffect(() => {
    let cancelled = false;
    fetchOptimizeHistory(HISTORY_FETCH_LIMIT)
      .then((data) => {
        if (!cancelled) setHistory(data);
      })
      .catch((e) => {
        if (!cancelled) setError(userFacingError(e, t));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [t]);

  const totalPages = Math.max(1, Math.ceil(history.length / HISTORY_PAGE_SIZE));
  const safePage = Math.min(page, totalPages);
  const paged = history.slice((safePage - 1) * HISTORY_PAGE_SIZE, safePage * HISTORY_PAGE_SIZE);

  const go = () => {
    if (!trimmed) return;
    navigate(`/optimization/${encodeURIComponent(trimmed)}`);
  };

  return (
    <main className="max-w-screen-xl mx-auto px-6 py-6 space-y-4">
      {/* ── Toolbar：沿用原会话列表页的工具栏结构（标题在左，操作在右）── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 p-4 flex flex-wrap items-center gap-3">
        <div>
          <h1 className="text-lg font-semibold text-gray-900">{t('nav.optimization')}</h1>
          <p className="text-xs text-gray-400 mt-0.5">{t('opt.entry.subtitle')}</p>
        </div>
        <div className="ml-auto flex items-center gap-2">
          <input
            type="text"
            aria-label={t('opt.entry.sessionIdLabel')}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') go();
            }}
            placeholder={t('opt.entry.sessionIdPlaceholder')}
            className="w-[320px] border border-gray-300 rounded-lg px-3 py-1.5 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-blue-400"
          />
          <button
            onClick={go}
            disabled={!trimmed}
            className="px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 transition-colors disabled:opacity-50 whitespace-nowrap"
          >
            {t('opt.session.action.start')}
          </button>
        </div>
      </div>

      {/* ── 历史分析记录 ── */}
      <div className="bg-white rounded-xl shadow-sm border border-gray-200 overflow-hidden">
        <div className="px-4 lg:px-6 py-3 border-b border-gray-200 bg-gray-50 flex items-center gap-2">
          <h2 className="text-sm font-semibold text-gray-700">{t('opt.history.title')}</h2>
          {!loading && !error && history.length > 0 && (
            <span className="text-xs text-gray-400">
              {t('opt.history.count', { n: history.length })}
              {history.length >= HISTORY_FETCH_LIMIT &&
                ` ${t('opt.history.limit', { n: HISTORY_FETCH_LIMIT })}`}
            </span>
          )}
        </div>

        {error ? (
          <div className="px-4 lg:px-6 py-4 text-sm text-red-700 bg-red-50 border-b border-red-200">
            {t('opt.history.loadFailed', { msg: error })}
          </div>
        ) : loading ? (
          <div className="flex items-center gap-3 py-16 justify-center text-gray-400">
            <Spinner size={20} />
            <span className="text-sm">{t('opt.history.loading')}</span>
          </div>
        ) : history.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-16 text-gray-400">
            <div className="text-4xl mb-3">📭</div>
            <p className="text-sm">{t('opt.history.empty')}</p>
          </div>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[720px]">
              <thead className="bg-gray-50 border-b border-gray-200">
                <tr>
                  <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                    {t('opt.history.col.sessionId')}
                  </th>
                  <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                    {t('opt.history.col.dimensions')}
                  </th>
                  <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                    {t('opt.history.col.firstAnalyzed')}
                  </th>
                  <th className="px-4 lg:px-6 py-3 text-left text-xs font-semibold text-gray-600 uppercase tracking-wide">
                    {t('opt.history.col.lastUpdated')}
                  </th>
                </tr>
              </thead>
              <tbody className="divide-y divide-gray-100">
                {paged.map((h) => (
                  <tr
                    key={h.session_id}
                    className="hover:bg-blue-50 transition-colors cursor-pointer"
                    onClick={() => navigate(`/optimization/${encodeURIComponent(h.session_id)}`)}
                  >
                    <td className="px-4 lg:px-6 py-3.5">
                      <span className="font-mono text-sm text-gray-800" title={h.session_id}>
                        {shortId(h.session_id)}
                      </span>
                    </td>
                    <td className="px-4 lg:px-6 py-3.5">
                      <div className="flex flex-wrap gap-1">
                        {h.dimensions.length === 0 ? (
                          <span className="text-xs text-gray-400">-</span>
                        ) : (
                          h.dimensions.map((d) => (
                            <span
                              key={d}
                              className={`px-2 py-0.5 rounded text-xs font-medium ${
                                DIM_TAG_CLS[d] ?? 'bg-gray-100 text-gray-600'
                              }`}
                            >
                              {DIM_LABELS[d] ? t(DIM_LABELS[d]) : d}
                            </span>
                          ))
                        )}
                      </div>
                    </td>
                    <td className="px-4 lg:px-6 py-3.5 text-sm text-gray-500 whitespace-nowrap">
                      {fmtNs(h.created_at_ns, locale)}
                    </td>
                    <td className="px-4 lg:px-6 py-3.5 text-sm text-gray-500 whitespace-nowrap">
                      {fmtNs(h.updated_at_ns, locale)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </div>

      {/* ── 分页（复用 AgentSessionsPage 的控件形态）── */}
      {!loading && !error && history.length > HISTORY_PAGE_SIZE && (
        <div className="flex items-center justify-between text-sm text-gray-600">
          <span>
            {t('opt.history.paginationSummary', { count: history.length, page: safePage, total: totalPages })}
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
    </main>
  );
}

// ── 主页面 ────────────────────────────────────────────────────────────────────

export const OptimizationPage: React.FC = () => {
  const { sessionId } = useParams<{ sessionId: string }>();

  return sessionId ? <SessionAnalysisView sessionId={sessionId} /> : <SessionEntryView />;
};
