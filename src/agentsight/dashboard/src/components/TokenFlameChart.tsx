import React, { useMemo, useRef, useState, useEffect } from 'react';
import type { CostStats, LLMCall, WasteItem, WasteReport } from '../types/optimization';
import { copyText } from './CopyButton';
import { useI18n } from '../i18n';
import type { MessageKey } from '../i18n';

type DimState = 'idle' | 'loading' | 'done' | 'error';

// ── 类别定义（与后端 LlmCall 字段一一对应）────────────────────────────────────
// 上柱：Context Window 组成（5 类，静态区域合并为一类）
type Cat = { key: string; labelKey: MessageKey; color: string; get: (c: LLMCall) => number };

// 配色：agentsight 亮色主题（Tailwind 系）——冷（人类输入/静态）在下，暖（重放历史）在上。
const CONTEXT_CATS: Cat[] = [
  { key: 'static', labelKey: 'flame.ctx.static', color: '#6b7280', get: c => c.system_prompt + c.skill_definitions + c.tool_definitions }, // gray-500 · 恒定基岩
  { key: 'user', labelKey: 'flame.ctx.user', color: '#3b82f6', get: c => c.user_messages },        // primary 蓝 · 人类输入
  { key: 'assistant', labelKey: 'flame.ctx.assistant', color: '#f59e0b', get: c => c.assistant_messages }, // amber-500 · O(n) 累积
  { key: 'tool', labelKey: 'flame.ctx.tool', color: '#10b981', get: c => c.tool_results },            // emerald-500 · 沉积大头
  { key: 'others', labelKey: 'flame.ctx.others', color: '#8b5cf6', get: c => c.injected_context },          // violet-500 · 注入
];

const OUTPUT_COLOR = '#ef4444'; // 输出曲线（danger 红）
const SAVE = '#10b981';         // 节省强调色（success 绿，浪费诊断表空态用）
const ROSE = '#ec4899';         // 浪费类型缺省色（pink-500）
const AXIS = '#9ca3af';         // gray-400 轴文字
const GRID = '#e5e7eb';         // gray-200 网格线

// SVG 几何常量（滚动定位与渲染共用）
const MIN_SLOT = 26;            // 最小槽宽（多步时滚动）
const ML = 48;                  // 左轴宽（固定列，不随横向滚动）
const PL = 6;                   // 滚动区左内边距

// ── 工具函数 ──────────────────────────────────────────────────────────────────
const fmtK = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k` : `${Math.round(n)}`);
const ctxTotal = (c: LLMCall) => CONTEXT_CATS.reduce((s, cat) => s + cat.get(c), 0);

// ── Summary Card ──────────────────────────────────────────────────────────────
function Card({ label, value, sub, accent, onClick }: { label: string; value: string; sub?: React.ReactNode; accent?: string; onClick?: () => void }) {
  return (
    <div
      className={`bg-white rounded-lg shadow border border-gray-200 px-4 py-3.5${onClick ? ' cursor-pointer hover:border-blue-300' : ''}`}
      onClick={onClick}
    >
      <div className="font-mono text-[10px] tracking-widest uppercase text-gray-500">{label}</div>
      <div className="text-2xl font-bold mt-1.5 leading-tight" style={{ color: accent ?? '#111827' }}>{value}</div>
      {sub && <div className="text-[11px] text-gray-500 mt-1 font-mono">{sub}</div>}
    </div>
  );
}

const Spinner: React.FC<{ size?: number }> = ({ size = 18 }) => (
  <span
    className="inline-block rounded-full border-2 border-blue-500 border-t-transparent animate-spin flex-shrink-0"
    style={{ width: size, height: size }}
  />
);

// ── 主组件 ────────────────────────────────────────────────────────────────────
export default function TokenFlameChart({ cost, waste, wasteState }: { cost: CostStats; waste?: WasteReport | null; wasteState?: DimState }) {
  const calls = cost.calls ?? [];
  const [selected, setSelected] = useState<number>(() => Math.max(0, calls.length - 1));
  const { t } = useI18n();

  // 派生聚合（全部 hooks 必须在早退之前调用）
  const derived = useMemo(() => {
    // 空类别（整条轨迹求和为 0）不显示
    const visCtx = CONTEXT_CATS.filter(cat => calls.some(c => cat.get(c) > 0));

    const inputTokens = calls.reduce((s, c) => s + ctxTotal(c), 0); // 计费口径 input（含重放）
    const outputTokens = calls.reduce((s, c) => s + c.output_tokens, 0);

    let peakIdx = 0;
    let peakVal = 0;
    calls.forEach((c, i) => { const t = ctxTotal(c); if (t > peakVal) { peakVal = t; peakIdx = i; } });

    const maxCtx = Math.max(1, ...calls.map(ctxTotal));
    const maxOut = Math.max(1, ...calls.map(c => c.output_tokens));

    return { visCtx, inputTokens, outputTokens, peakIdx, peakVal, maxCtx, maxOut };
  }, [calls]);

  // SVG 几何（自适应容器宽度）— hooks 需在早退前调用
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerW, setContainerW] = useState(600);
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const ro = new ResizeObserver(entries => {
      for (const e of entries) setContainerW(e.contentRect.width);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 初始 / 切换选中步时，把视口滚动到选中柱（长会话峰值在末尾，初始 scrollLeft=0 会看不到峰值）
  useEffect(() => {
    const el = containerRef.current;
    if (!el || calls.length === 0) return;
    const slot = Math.max(MIN_SLOT, Math.floor((containerW - PL - 16) / calls.length));
    const target = PL + selected * slot + slot / 2 - el.clientWidth / 2;
    el.scrollTo({ left: Math.max(0, target), behavior: 'smooth' });
  }, [selected, containerW, calls.length]);

  // 空状态
  if (calls.length === 0) {
    return (
      <div className="bg-white rounded-lg shadow border border-gray-200 p-5 mb-4">
        <h3 className="text-sm font-semibold text-gray-700">{t('flame.title')}</h3>
        <p className="text-gray-400 font-mono text-[13px] mt-2">{t('flame.empty.noPerStepData')}</p>
      </div>
    );
  }

  const sel = calls[Math.min(selected, calls.length - 1)];

  const { visCtx, maxCtx, maxOut } = derived;
  const n = calls.length;
  const SLOT = Math.max(MIN_SLOT, Math.floor((containerW - PL - 16) / n)); // 自适应槽宽
  const BARW = Math.min(SLOT - 10, Math.max(16, SLOT * 0.6));         // 柱宽随槽宽缩放
  const H_TOP = 26;                       // 输出标注区
  const H_UP = 260;                       // 上柱区
  const scale = H_UP / maxCtx;            // px / token
  const H_X = 34;                         // x 轴标签
  const chartW = PL + n * SLOT + 16;
  // 下柱区（可优化 · 可省镜像柱）已删：数据源为未经 LLM 判定的启发式估算字段，
  // 与浪费诊断表口径矛盾，与 Headroom 卡/Suggestions 同批清理。
  const svgH = H_TOP + H_UP + H_X;
  const baseY = H_TOP + H_UP;             // 基线 y

  return (
    <div className="mb-5">
      {/* 1 · Summary Cards（「LLM 判定可省」卡已删：汇总数字为伪精度，加载/失败状态由下方浪费诊断表承担） */}
      <div className="grid grid-cols-2 gap-3 mb-4">
        <Card
          label={t('flame.summary.totalTokens')}
          value={fmtK(derived.inputTokens + derived.outputTokens)}
          sub={t('flame.summary.totalTokensSub', { input: fmtK(derived.inputTokens), output: fmtK(derived.outputTokens) })}
        />
        <Card
          label={t('flame.summary.peakContext')}
          value={fmtK(derived.peakVal)}
          sub={t('flame.summary.peakContextSub', { step: derived.peakIdx, time: calls[derived.peakIdx].time })}
          accent="#3b82f6"
          onClick={() => setSelected(derived.peakIdx)}
        />
      </div>

      {/* 2 · 主图表 + 3 · Detail Panel（两列等高：默认 stretch，右侧面板随图表卡片拉伸） */}
      <div className="grid gap-3 mb-4" style={{ gridTemplateColumns: '1fr minmax(240px, 288px)' }}>
        <div className="bg-white rounded-lg shadow border border-gray-200 p-3.5 overflow-hidden">
          <div className="flex justify-between items-baseline mb-2 flex-wrap gap-2">
            <h3 className="text-sm font-semibold text-gray-700 m-0">
              {t('flame.chart.title')} <span className="font-normal text-gray-400">· {t('flame.chart.subtitle')}</span>
            </h3>
            {/* 图例 */}
            <div className="flex gap-3 flex-wrap">
              {visCtx.map(cat => (
                <span key={cat.key} className="inline-flex items-center gap-1.5 text-[11px] text-gray-500">
                  <i className="w-[9px] h-[9px] rounded-sm" style={{ background: cat.color }} />{t(cat.labelKey)}
                </span>
              ))}
              <span className="inline-flex items-center gap-1.5 text-[11px]" style={{ color: OUTPUT_COLOR }}>
                <i className="w-3.5 h-[2.5px] rounded-sm" style={{ background: OUTPUT_COLOR }} />{t('flame.legend.outputTokens')}
              </span>
            </div>
          </div>

          <div className="flex items-stretch">
            {/* 固定左栏：标注上区语义（不随横向滚动移动） */}
            <div className="relative w-[22px] flex-shrink-0" style={{ height: svgH }}>
              <div className="absolute left-0 w-[22px] flex items-center justify-center" style={{ top: H_TOP, height: H_UP }}>
                <span className="font-mono text-[10px] tracking-widest text-gray-500 whitespace-nowrap" style={{ writingMode: 'vertical-rl' }}>{t('flame.axis.contextLabel')}</span>
              </div>
            </div>
            {/* 固定 y 轴刻度（不随横向滚动移动，否则向右滚时刻度不可见） */}
            <svg width={ML} height={svgH} className="block flex-shrink-0">
              {[0, 0.25, 0.5, 0.75, 1].map(f => {
                const y = baseY - f * H_UP;
                return (
                  <text key={f} x={ML - 6} y={y + 3} textAnchor="end" fontSize={9} fill={AXIS} fontFamily="ui-monospace, monospace">{fmtK(maxCtx * f)}</text>
                );
              })}
              {/* baseline 标注 */}
              <text x={ML - 6} y={baseY + 3} textAnchor="end" fontSize={9} fill="#6b7280" fontFamily="ui-monospace, monospace">0</text>
            </svg>
            <div ref={containerRef} className="overflow-x-auto pb-1 flex-1">
              <svg viewBox={`0 0 ${chartW} ${svgH}`} width={chartW} height={svgH} className="block" style={{ maxWidth: 'none' }}>
                {/* y 轴网格线（刻度文字在左侧固定 SVG） */}
                {[0, 0.25, 0.5, 0.75, 1].map(f => {
                  const y = baseY - f * H_UP;
                  return (
                    <line key={f} x1={0} y1={y} x2={chartW - 8} y2={y} stroke={GRID} strokeWidth={0.6} strokeDasharray={f === 0 ? '' : '2 3'} />
                  );
                })}

                {calls.map((c, i) => {
                  const x = PL + i * SLOT + (SLOT - BARW) / 2;
                  const isSel = i === selected;
                  // 上柱：从 baseline 向上堆叠
                  let yUp = baseY;
                  const upSegs = visCtx.map(cat => {
                    const v = cat.get(c);
                    const h = v * scale;
                    yUp -= h;
                    return { cat, v, y: yUp, h };
                  });
                  return (
                    <g key={i} onClick={() => setSelected(i)} style={{ cursor: 'pointer' }} opacity={isSel || selected < 0 ? 1 : 0.82}>
                      {/* 选中背景高亮 */}
                      {isSel && <rect x={PL + i * SLOT} y={2} width={SLOT} height={svgH - H_X + 4} fill="rgba(59,130,246,.08)" />}
                      {/* 上柱段 */}
                      {upSegs.map((s, k) => s.h > 0 && (
                        <rect key={k} x={x} y={s.y} width={BARW} height={s.h} fill={s.cat.color} opacity={isSel ? 1 : 0.9} />
                      ))}
                      {/* x 轴标签（稀疏） */}
                      {(i % Math.ceil(n / 24 || 1) === 0 || isSel) && (
                        <text x={x + BARW / 2} y={svgH - 12} textAnchor="middle" fontSize={8} fill={isSel ? '#111827' : AXIS} fontFamily="ui-monospace, monospace">{i}</text>
                      )}
                    </g>
                  );
                })}
                {/* 基线 */}
                <line x1={0} y1={baseY} x2={chartW - 8} y2={baseY} stroke="#6b7280" strokeWidth={1} />
                {/* 输出 token 曲线（叠加在柱状图上方，独立缩放） */}
                {n >= 2 && (() => {
                  const outScale = (H_UP * 0.85) / maxOut;
                  const pts = calls.map((c, i) => {
                    const cx = PL + i * SLOT + SLOT / 2;
                    const cy = baseY - c.output_tokens * outScale;
                    return `${cx},${cy}`;
                  }).join(' ');
                  return (
                    <g>
                      <polyline points={pts} fill="none" stroke={OUTPUT_COLOR} strokeWidth={1.6} strokeLinejoin="round" strokeLinecap="round" opacity={0.85} />
                      {calls.map((c, i) => {
                        const cx = PL + i * SLOT + SLOT / 2;
                        const cy = baseY - c.output_tokens * outScale;
                        return <circle key={i} cx={cx} cy={cy} r={i === selected ? 3.5 : 2} fill={OUTPUT_COLOR} stroke="#fff" strokeWidth={i === selected ? 1.2 : 0.6} opacity={i === selected ? 1 : 0.7} />;
                      })}
                    </g>
                  );
                })()}
              </svg>
            </div>
          </div>
        </div>

        {/* 3 · Detail Panel */}
        <DetailPanel c={sel} visCtx={visCtx} />
      </div>

      {/* 4 · 浪费诊断表（Rust 供数 → LLM 判定是否值得优化）*/}
      <WasteTable waste={waste} wasteState={wasteState} />
    </div>
  );
}

// ── 浪费诊断表 ──────────────────────────────────────────────────────────────────
function wasteColor(optimization: string): string {
  const o = optimization;
  if (o.includes('\u7f13\u5b58') || o.includes('Cache')) return '#6366f1';
  if (o.includes('\u5386\u53f2') || o.includes('History')) return '#14b8a6';
  if (o.includes('\u5de5\u5177\u8f93\u51fa') || o.includes('Trim')) return '#eab308';
  if (o.includes('\u6ce8\u5165') || o.includes('Context')) return '#fb923c';
  if (o.includes('\u63d0\u793a\u8bcd\u538b\u7f29') || o.includes('Compression')) return '#3b82f6';
  if (o.includes('\u7ecf\u9a8c')) return '#8b5cf6';   // 试错型 · violet-500
  if (o.includes('\u89c4\u8303')) return '#0ea5e9';   // 返工型 · sky-500
  return ROSE;
}

// 置信度 badge 配色（与准确性 IssueTable 一致）
const CONF_CLS: Record<string, string> = {
  '\u9ad8': 'bg-green-100 text-green-700',
  '\u4e2d': 'bg-yellow-100 text-yellow-700',
  '\u4f4e': 'bg-gray-100 text-gray-500',
};

/** 经验条目的可复制文本 —— 直接贴进 Skill / 规范文件。 */
function experienceText(t: (key: MessageKey, params?: Record<string, string | number>) => string, it: WasteItem): string {
  const e = it.experience;
  if (!e) return '';
  const lines: string[] = [];
  if (e.applicability) lines.push(`${t('flame.exp.applicability')}${e.applicability}`);
  if (e.pitfall) lines.push(`${t('flame.exp.pitfall')}${e.pitfall}`);
  if (e.effective_path) lines.push(`${t('flame.exp.effectivePath')}${e.effective_path}`);
  if (e.rule) lines.push(`${t('flame.exp.rule')}${e.rule}`);
  if (e.bad_example) lines.push(`${t('flame.exp.badExample')}${e.bad_example}`);
  if (e.good_example) lines.push(`${t('flame.exp.goodExample')}${e.good_example}`);
  if (e.scope) lines.push(`${t('flame.exp.scope')}${e.scope}`);
  return lines.join('\n');
}

/** 优化提示词 —— 有经验用经验全文；token 浪费类无 experience 字段，由现象/手段/证据合成。 */
function promptText(t: (key: MessageKey, params?: Record<string, string | number>) => string, it: WasteItem): string {
  const exp = experienceText(t, it);
  if (exp) return exp;
  const lines = [`${t('flame.exp.symptom')}${it.symptom}`, `${t('flame.exp.optimization')}${it.optimization}`];
  if (it.evidence) lines.push(`${t('flame.exp.evidence')}${it.evidence}`);
  return lines.join('\n');
}

/** 展开详情行：完整证据 + 可沉淀经验（结构与准确性表的展开区一致）。 */
function WasteDetailRow({ it }: { it: WasteItem }) {
  const { t } = useI18n();
  const text = experienceText(t, it);

  return (
    <tr className="bg-gray-50">
      <td colSpan={5} className="px-4 py-3">
        <dl className="text-sm space-y-2">
          <div>
            <dt className="text-xs font-semibold text-gray-500 uppercase tracking-wide">{t('flame.detail.evidence')}</dt>
            <dd className="mt-1 font-mono text-xs text-gray-700 whitespace-pre-wrap leading-relaxed">{it.evidence}</dd>
          </div>
          {text && (
            <div>
              <dt className="text-xs font-semibold text-gray-500 uppercase tracking-wide">{t('flame.detail.experience')}</dt>
              <dd className="mt-1">
                <div className="font-mono text-xs text-gray-700 whitespace-pre-wrap leading-relaxed">{text}</div>
                {it.steps && it.steps.length > 0 && (
                  <div className="font-mono text-[10px] text-gray-400 mt-1.5">
                    {t('flame.detail.stepsNote', { steps: it.steps.length })}
                  </div>
                )}
              </dd>
            </div>
          )}
        </dl>
      </td>
    </tr>
  );
}

// 优化提示词复制按钮（与准确性表同款）
function ExpCopyBtn({ text }: { text: string }) {
  const [done, setDone] = useState(false);
  const { t } = useI18n();
  // HTTP 非安全上下文无 navigator.clipboard，copyText 内部自动降级
  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    copyText(text, () => {
      setDone(true);
      setTimeout(() => setDone(false), 2000);
    });
  };
  return (
    <button
      onClick={handleCopy}
      className={`px-2 py-1 rounded text-xs whitespace-nowrap transition-colors ${
        done
          ? 'bg-green-100 text-green-600'
          : 'bg-gray-100 hover:bg-gray-200 text-gray-500 hover:text-gray-700'
      }`}
      title={done ? t('common.copied') : t('flame.copyPromptTitle')}
    >
      {done ? t('common.copied') : `⧉ ${t('common.copy')}`}
    </button>
  );
}

function WasteTable({ waste, wasteState }: { waste?: WasteReport | null; wasteState?: DimState }) {
  const [open, setOpen] = useState<number | null>(0);
  const { t } = useI18n();
  // 加载中 / 失败 / 未分析态
  if (wasteState !== 'done' || !waste) {
    return (
      <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
        <h3 className="text-sm font-semibold text-gray-700">{t('flame.waste.title')}</h3>
        {wasteState === 'error' ? (
          <p className="text-red-500 font-mono text-[13px] mt-2">{t('flame.waste.error')}</p>
        ) : wasteState === 'loading' ? (
          <div className="flex items-center gap-3 py-2 mt-1">
            <Spinner />
            <span className="text-gray-500 font-mono text-[13px]">{t('flame.waste.loading')}</span>
          </div>
        ) : (
          <p className="text-gray-400 font-mono text-[13px] mt-2">{t('flame.waste.notAnalyzed')}</p>
        )}
      </div>
    );
  }

  if (waste.items.length === 0) {
    return (
      <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
        <h3 className="text-sm font-semibold text-gray-700">{t('flame.waste.title')}</h3>
        {waste.considered === 0 ? (
          <p className="text-gray-500 font-mono text-[13px] mt-2">{t('flame.waste.noCandidates')}</p>
        ) : (
          <p className="font-mono text-[13px] mt-2" style={{ color: SAVE }}>
            {t('flame.waste.noWasteFound', { count: waste.considered })}
          </p>
        )}
      </div>
    );
  }

  return (
    <div className="bg-white rounded-lg shadow border border-gray-200 p-5">
      <h3 className="text-sm font-semibold text-gray-700 m-0">{t('flame.waste.title')}</h3>
      <div className="overflow-x-auto mt-3.5">
        <table className="w-full min-w-[720px] text-sm">
          <thead>
            <tr className="border-b border-gray-200">
              <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600">{t('flame.waste.col.symptom')}</th>
              <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">{t('flame.waste.col.type')}</th>
              <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">{t('flame.waste.col.optimization')}</th>
              <th className="text-left pb-2 pr-3 text-xs font-semibold text-gray-600 whitespace-nowrap">{t('flame.waste.col.confidence')}</th>
              <th className="text-left pb-2 text-xs font-semibold text-gray-600 whitespace-nowrap">{t('flame.waste.col.prompt')}</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-gray-100">
            {waste.items.map((it, i) => {
              const isOpen = open === i;
              const expText = promptText(t, it);
              return (
                <React.Fragment key={i}>
                  <tr
                    className={`cursor-pointer transition-colors ${isOpen ? 'bg-blue-50' : 'hover:bg-gray-50'}`}
                    onClick={() => setOpen(isOpen ? null : i)}
                  >
                    <td className="py-2.5 pr-3 text-gray-800">
                      <span className="text-gray-400 text-xs mr-1.5">{isOpen ? '▼' : '▶'}</span>
                      {it.symptom}
                    </td>
                    <td className="py-2.5 pr-3 whitespace-nowrap">
                      <span className="inline-flex items-center gap-1.5">
                        <i className="w-[9px] h-[9px] rounded-sm flex-shrink-0" style={{ background: wasteColor(it.optimization) }} />
                        <span className="px-1.5 py-0.5 rounded bg-gray-100 text-gray-700 font-mono text-xs">{it.subtype}</span>
                      </span>
                    </td>
                    <td className="py-2.5 pr-3 text-xs text-gray-700">{it.optimization}</td>
                    <td className="py-2.5 pr-3 whitespace-nowrap">
                      {(() => {
                        const raw = it.confidence;
                        const key =
                          raw === '\u9ad8'
                            ? 'flame.confidence.high'
                            : raw === '\u4e2d'
                            ? 'flame.confidence.medium'
                            : raw === '\u4f4e'
                            ? 'flame.confidence.low'
                            : null;
                        const label = key ? t(key as MessageKey) : raw;
                        const cls = CONF_CLS[raw] ?? CONF_CLS['\u4f4e'];
                        return (
                          <span className={`px-1.5 py-0.5 rounded text-xs font-medium ${cls}`}>
                            {label}
                          </span>
                        );
                      })()}
                    </td>
                    <td className="py-2.5 whitespace-nowrap">
                      <ExpCopyBtn text={expText} />
                    </td>
                  </tr>
                  {isOpen && <WasteDetailRow it={it} />}
                </React.Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

// ── Detail Panel ──────────────────────────────────────────────────────────────
function DetailPanel({ c, visCtx }: {
  c: LLMCall; visCtx: Cat[];
}) {
  const { t } = useI18n();
  const total = ctxTotal(c);

  return (
    <div className="bg-white rounded-lg shadow border border-gray-200 p-4 h-full flex flex-col">
      <div className="flex justify-between items-baseline">
        <h3 className="text-sm font-semibold text-gray-900 m-0">{t('flame.detail.stepTitle', { id: c.step_id })}</h3>
        <span className="font-mono text-[11px] text-gray-500">{c.time} · {c.label}</span>
      </div>

      {/* 各类别 token + 百分比（flex-1 垂直居中，吸收等高拉伸后的空白） */}
      <div className="flex-1 flex flex-col justify-center gap-[10px] py-3">
        {visCtx.map(cat => {
          const v = cat.get(c);
          const pct = total > 0 ? (v / total) * 100 : 0;
          return (
            <div key={cat.key} className="grid items-center gap-2 text-xs" style={{ gridTemplateColumns: '10px 1fr auto' }}>
              <i className="w-[9px] h-[9px] rounded-sm" style={{ background: cat.color }} />
              <span className="text-gray-700">{t(cat.labelKey)}</span>
              <span className="font-mono text-[11px] text-gray-500">{fmtK(v)} · {pct.toFixed(0)}%</span>
            </div>
          );
        })}
        <div className="grid items-center gap-2 text-xs border-t border-gray-200 pt-2.5 mt-1" style={{ gridTemplateColumns: '10px 1fr auto' }}>
          <i className="w-[9px] h-[9px] rounded-sm" style={{ background: OUTPUT_COLOR }} />
          <span className="text-gray-700">{t('flame.detail.currentOutput')}</span>
          <span className="font-mono text-[11px]" style={{ color: OUTPUT_COLOR }}>↑{fmtK(c.output_tokens)}</span>
        </div>
      </div>

      {/* 汇总钉底 */}
      <div className="text-[11px] font-mono text-gray-500 border-t border-gray-200 pt-2.5">
        prompt_tokens ≈ <b className="text-gray-900">{fmtK(total)}</b>
      </div>
    </div>
  );
}
