import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import {
  fetchAgentHealth,
  fetchAgentProcessHealth,
  deleteAgentHealth,
  restartAgentHealth,
  fetchInterruptions,
  resolveInterruption,
  createCredentialBinding,
  fetchAgentProtectionPreview,
  fetchEnforcementBindings,
  detachEnforcementBinding,
  fetchSecurityCases,
  fetchLatencyMetrics,
} from '../utils/apiClient';
import type { InterruptionRecord, InterruptionSeverity, LatencyMetricsSummary, MetricPercentiles } from '../utils/apiClient';
import type { AgentActivitySummary, AgentHealthStatus } from '../types';
import { useI18n, useLocaleTag, INTERRUPTION_TYPES, interruptionTypeKey } from '../i18n';
import type { MessageKey } from '../i18n';
import { formatNs } from '../utils/datetime';
import { CREDENTIAL_POLICY_ID } from '../constants/policyIds';

// ─── Agent status section ─────────────────────────────────────────────────────

/** Status dot color mapping */
const STATUS_COLORS: Record<string, string> = {
  healthy: 'bg-green-500',
  unhealthy: 'bg-red-500',
  hung: 'bg-orange-500',
  unknown: 'bg-yellow-400',
  no_port: 'bg-gray-400',
  offline: 'bg-gray-500',
};

/** Status display label key */
const STATUS_LABEL_KEY: Record<string, MessageKey> = {
  healthy: 'ah.status.healthy',
  unhealthy: 'ah.status.unhealthy',
  hung: 'ah.status.hung',
  unknown: 'ah.status.unknown',
  no_port: 'ah.status.noPort',
  offline: 'ah.status.offline',
};

/** Status tooltip key */
const STATUS_TOOLTIP_KEY: Record<string, MessageKey> = {
  healthy: 'ah.tooltip.healthy',
  unhealthy: 'ah.tooltip.unhealthy',
  hung: 'ah.tooltip.hung',
  unknown: 'ah.tooltip.unknown',
  no_port: 'ah.tooltip.noPort',
  offline: 'ah.tooltip.offline',
};

type AgentStatus = AgentHealthStatus['status'];

const AGGREGATE_SEVERITY_RANK: Record<AgentStatus, number> = {
  unknown: 0,
  no_port: 1,
  healthy: 2,
  offline: 3,
  unhealthy: 4,
  hung: 5,
};

const CARD_STATUS_ORDER: Record<AgentStatus, number> = {
  hung: 0,
  unhealthy: 1,
  healthy: 2,
  no_port: 3,
  unknown: 4,
  offline: 5,
};

interface AgentGroup {
  agentName: string;
  agents: AgentHealthStatus[];
  status: AgentStatus;
}

function getAggregateStatus(group: AgentHealthStatus[]): AgentStatus {
  let worst: AgentStatus = 'unknown';

  for (const agent of group) {
    if (AGGREGATE_SEVERITY_RANK[agent.status] > AGGREGATE_SEVERITY_RANK[worst]) {
      worst = agent.status;
    }
  }

  return worst;
}

function groupAgentsByName(agents: AgentHealthStatus[]): AgentGroup[] {
  const groups = new Map<string, AgentHealthStatus[]>();

  for (const agent of agents) {
    const group = groups.get(agent.agent_name);
    if (group) {
      group.push(agent);
    } else {
      groups.set(agent.agent_name, [agent]);
    }
  }

  return Array.from(groups, ([agentName, group]) => ({
    agentName,
    agents: group,
    status: getAggregateStatus(group),
  }));
}

function isPromotedGatewayRunning(agent: AgentHealthStatus): boolean {
  return (
    agent.role === 'gateway' &&
    agent.status === 'no_port' &&
    (agent.ports?.length ?? 0) === 0
  );
}

/** Format relative time */
function relativeTime(timestampMs: number, t: (key: MessageKey, params?: Record<string, string | number>) => string): string {
  if (timestampMs === 0) return '—';
  const diff = Math.floor((Date.now() - timestampMs) / 1000);
  if (diff < 5) return t('common.justNow');
  if (diff < 60) return t('common.secondsAgo', { n: diff });
  if (diff < 3600) return t('common.minutesAgo', { n: Math.floor(diff / 60) });
  return t('common.hoursAgo', { n: Math.floor(diff / 3600) });
}

interface Toast {
  id: number;
  message: string;
}

/**
 * 归一化「保护目录」默认值：仅接受绝对路径且非文件系统根 `/`。
 * 拿不到有效工作目录（空、相对路径或根 `/`）时返回空串，交给 placeholder
 * 提示用户手动填写/扫描——绝不 fallback 到 `/`，否则会被后端按
 * "protection directory cannot be the filesystem root" 拒绝。
 */
function defaultProtectionDir(path?: string | null): string {
  const trimmed = (path ?? '').trim();
  if (!trimmed || trimmed === '/' || !trimmed.startsWith('/')) return '';
  return trimmed;
}
const ActivityMetric: React.FC<{ label: string; value: number; locale: string }> = ({
  label,
  value,
  locale,
}) => (
  <div>
    <dt className="text-gray-400">{label}</dt>
    <dd className="mt-0.5 text-base font-semibold text-gray-800">
      {value.toLocaleString(locale)}
    </dd>
  </div>
);

const AgentActivitySection: React.FC = () => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [agents, setAgents] = useState<AgentActivitySummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const hasDataRef = useRef(false);

  const refresh = useCallback(async () => {
    try {
      const data = await fetchAgentHealth();
      setAgents(Array.isArray(data?.agents) ? data.agents : []);
      setWarnings(Array.isArray(data?.warnings) ? data.warnings : []);
      setError(null);
      hasDataRef.current = true;
    } catch (requestError: unknown) {
      if (!hasDataRef.current) {
        setError(requestError instanceof Error ? requestError.message : t('ah.requestFailed'));
      }
    } finally {
      setLoading(false);
    }
  }, [t]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(refresh, 30_000);
    return () => clearInterval(timer);
  }, [refresh]);

  return (
    <section>
      <div className="flex items-center justify-between mb-3">
        <h2
          className="text-lg font-semibold text-gray-800 cursor-help"
          title={t('ah.agentDashboardTooltip')}
        >
          {t('ah.agentDashboard')}
        </h2>
        {!loading && !error && (
          <span className="text-xs px-2 py-1 rounded-full bg-gray-100 text-gray-600">
            {t('ah.observedAgents', { n: agents.length })}
          </span>
        )}
      </div>

      {loading ? (
        <div className="py-8 text-center text-sm text-gray-400">{t('common.loading')}</div>
      ) : error ? (
        <div className="py-8 text-center text-sm text-red-400">{error}</div>
      ) : agents.length === 0 ? (
        <div className="py-8 text-center text-sm text-gray-400 bg-white rounded-lg border border-gray-200">
          {t('ah.noAgents')}
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3">
          {agents.map(agent => (
            <article
              key={agent.agent_name.toLowerCase()}
              className="rounded-lg border border-gray-200 bg-white shadow-sm p-4"
            >
              <div className="flex items-start justify-between gap-3">
                <h3 className="font-semibold text-sm text-gray-900 truncate">
                  {agent.agent_name}
                </h3>
                <div className="flex flex-wrap justify-end gap-1">
                  {agent.source.split('+').map(source => (
                    <span
                      key={source}
                      className="text-[10px] px-1.5 py-0.5 rounded-full bg-blue-50 text-blue-700"
                    >
                      {source === 'genai_events' ? 'GenAI' : 'Trajectory'}
                    </span>
                  ))}
                </div>
              </div>
              <dl className="mt-4 grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
                {agent.source.includes('genai_events') && (
                  <>
                    <ActivityMetric label={t('ah.genaiCalls')} value={agent.genai_calls} locale={locale} />
                    <ActivityMetric label={t('ah.genaiTokens')} value={agent.genai_tokens} locale={locale} />
                  </>
                )}
                {agent.source.includes('trajectories') && (
                  <>
                    <ActivityMetric label={t('ah.trajectorySteps')} value={agent.trajectory_steps} locale={locale} />
                    <ActivityMetric label={t('ah.trajectoryTokens')} value={agent.trajectory_tokens} locale={locale} />
                  </>
                )}
                <div className="col-span-2 pt-2 border-t border-gray-100">
                  <dt className="text-gray-400">{t('ah.lastSeen')}</dt>
                  <dd className="mt-0.5 text-gray-700">
                    {agent.last_seen_ns > 0 ? formatNs(agent.last_seen_ns, locale) : '—'}
                  </dd>
                </div>
              </dl>
            </article>
          ))}
        </div>
      )}
      {warnings.length > 0 && (
        <div className="mt-3 rounded border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-700">
          {t('ah.partialData', { sources: warnings.join(', ') })}
        </div>
      )}
    </section>
  );
};

function formatMetricValue(value: number): string {
  return value.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

function formatMetricP50(metric: MetricPercentiles | null, unit: string): string {
  return metric ? formatMetricValue(metric.p50) + ' ' + unit : '—';
}

function canonicalAgentKey(agentName: string): string {
  return agentName.toLowerCase();
}

const LatencyMetricsRow: React.FC<{ metrics: LatencyMetricsSummary }> = ({ metrics }) => {
  const { t } = useI18n();
  const items = [
    { label: t('latency.ttft'), metric: metrics.ttft_ms, unit: 'ms' },
    { label: t('latency.tps'), metric: metrics.tps_tokens_per_second, unit: 'tokens/s' },
    { label: t('latency.tpot'), metric: metrics.tpot_ms_per_token, unit: 'ms/token' },
    { label: t('latency.e2e'), metric: metrics.e2e_latency_ms, unit: 'ms' },
  ];

  if (!items.some(item => item.metric !== null)) return null;

  const tooltip = items
    .map(({ label, metric, unit }) => {
      if (!metric) return label + ' —';
      return (
        label +
        ' ' +
        t('latency.p50') +
        ' ' +
        formatMetricValue(metric.p50) +
        ' ' +
        unit +
        ' · ' +
        t('latency.p95') +
        ' ' +
        formatMetricValue(metric.p95) +
        ' ' +
        unit +
        ' · ' +
        t('latency.p99') +
        ' ' +
        formatMetricValue(metric.p99) +
        ' ' +
        unit
      );
    })
    .join(' · ');

  return (
    <div
      className="mt-2 pt-2 border-t border-gray-100 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-gray-700"
      title={tooltip}
      aria-label={tooltip}
    >
      {items.map(({ label, metric, unit }) => (
        <span key={label}>
          <span className="text-gray-400">{label}</span> {formatMetricP50(metric, unit)}
        </span>
      ))}
    </div>
  );
};

const LATENCY_TIME_PRESETS = [
  { key: 'latency.range24h', ms: 24 * 3600 * 1000 },
  { key: 'latency.range7d', ms: 7 * 24 * 3600 * 1000 },
  { key: 'latency.range30d', ms: 30 * 24 * 3600 * 1000 },
] as const;

const AgentCard: React.FC<{
  agent: AgentHealthStatus;
  related: AgentHealthStatus[];
  onDelete: (pid: number) => void;
  onRestart: (pid: number) => void;
  restarting: boolean;
  protectedByPolicy?: boolean;
  bindingId?: string;
  pendingCaseCount?: number;
  onProtected?: (pid: number, bindingId: string) => void;
  onDetachProtection?: (pid: number, bindingId: string) => Promise<void> | void;
  addToast?: (message: string) => void;
  latency?: LatencyMetricsSummary;
  showProcessDetails?: boolean;
}> = ({
  agent,
  related,
  onDelete,
  onRestart,
  restarting,
  protectedByPolicy = false,
  bindingId,
  pendingCaseCount = 0,
  onProtected = () => undefined,
  onDetachProtection = () => undefined,
  addToast = () => undefined,
  latency,
  showProcessDetails = false,
}) => {
  const { t } = useI18n();
  const [showRelated, setShowRelated] = useState(false);
  const [showProtection, setShowProtection] = useState(false);
  const [protecting, setProtecting] = useState(false);
  const [detaching, setDetaching] = useState(false);
  const [directory, setDirectory] = useState(() => defaultProtectionDir(agent.workspace_path));
  const [sources, setSources] = useState<string[]>([]);
  const [trustedTarget, setTrustedTarget] = useState('');

  const loadProtectionDefaults = async (customDirectory?: string) => {
    const preview = await fetchAgentProtectionPreview(agent.pid, customDirectory);
    // 后端保证 workspace_path 非根，但仍归一化兜底：无效时回退到 agent 真实工作目录，绝不落到 `/`。
    setDirectory(defaultProtectionDir(preview.workspace_path) || defaultProtectionDir(agent.workspace_path));
    setSources(preview.source_paths);
    // 回填已有策略的可信目标，避免在保存时被清空丢失（P2）。仅当 preview 含可信目标时
    // 才覆盖，扫描无策略目录返回空数组时保留用户当前输入。
    const existingTrusted = (preview.trusted_endpoints ?? [])[0];
    if (existingTrusted) {
      setTrustedTarget(existingTrusted);
    }
    return preview;
  };

  // 「扫描」按钮：先做客户端校验，directory 经归一无效（空/根/相对）时不发后端请求，
  // 也不 fallback 到 workspace_path（那正是 `/` 的来源），直接给友好中文提示。
  const handleScan = () => {
    if (!defaultProtectionDir(directory)) {
      addToast('请先填写要保护的绝对路径目录');
      return;
    }
    void loadProtectionDefaults(directory).catch((error: any) => addToast(`目录扫描失败: ${error.message}`));
  };

  // 统一的弹窗打开入口：每次打开都按 agent 真实工作目录重新播种「保护目录」，
  // 避免停用保护后重开时沿用脏值或退化为 `/`（与首次开启行为一致）。
  const openProtectionDialog = () => {
    const seeded = defaultProtectionDir(agent.workspace_path);
    setDirectory(seeded);
    setShowProtection(true);
    // workspace_path 无效（空/根/相对）时不请求后端默认策略，避免后端用 `/`
    // 触发 400；直接打开弹窗让用户手填/扫描。
    if (seeded) void loadProtectionDefaults().catch(() => undefined);
  };

  const applyProtection = async (sourcePaths?: string[]) => {
    const selected = sourcePaths ?? sources;
    if (selected.length === 0) {
      setShowProtection(true);
      addToast('未发现敏感文件，请在设置中选择包含 .env 或凭据文件的目录');
      return;
    }
    setProtecting(true);
    try {
      // 原子替换（先建后删）：先创建新的凭据保护绑定，成功后再解绑旧绑定。
      // 严禁先删旧、再建新——一旦创建失败（503/校验/网络），旧绑定已被删除，
      // 会留下 Agent 静默失去保护的空窗（最坏失败模式）。
      const previousBindingId = bindingId;
      const created = await createCredentialBinding({
        agent_id: agent.agent_name,
        root_pid: agent.pid,
        source_paths: selected,
        trusted_endpoints: trustedTarget.trim() ? [trustedTarget.trim()] : [],
        revision: Date.now(),
        mode: 'audit',
        taint_ttl_secs: 900,
        destination_scope: 'public_ipv4',
      });
      // 新保护已生效，再清理旧绑定。删旧失败不算失败（新绑定已在，保护未中断），
      // 仅告警提示可稍后手动解绑，避免与已有 ActPlane binding 冲突/留下重复 binding。
      if (previousBindingId && previousBindingId !== created.request.binding_id) {
        try {
          await detachEnforcementBinding(previousBindingId);
        } catch (detachError: any) {
          addToast(
            `⚠️ 新保护已生效，但旧绑定清理失败：${detachError.message ?? '请稍后在设置中手动解绑'}`
          );
        }
      }
      onProtected(agent.pid, created.request.binding_id);
      setShowProtection(false);
      addToast('✅ 已开启审计保护：发现风险时记录证据，不阻断 Agent');
    } catch (error: any) {
      addToast(`开启失败: ${error.message ?? '请稍后重试'}`);
    } finally {
      setProtecting(false);
    }
  };

  const enableProtection = async () => {
    // workspace_path 无效（空/根/相对）时与「设置」一致：不调用自动生成默认策略
    // 的后端 API（否则后端用 `/` 当保护目录直接 400，弹窗都打不开），改为直接
    // 打开同一弹窗、目录留空 + placeholder，交用户手填/扫描。
    if (!defaultProtectionDir(agent.workspace_path)) {
      openProtectionDialog();
      return;
    }
    setProtecting(true);
    // 重开时先按 agent 真实工作目录重新播种，避免 preview 异常时沿用上一次脏值/根目录。
    setDirectory(defaultProtectionDir(agent.workspace_path));
    try {
      const preview = await loadProtectionDefaults();
      await applyProtection(preview.source_paths);
    } catch (error: any) {
      addToast(`无法生成默认策略: ${error.message ?? '请检查 Agent 工作目录'}`);
    } finally {
      setProtecting(false);
    }
  };

  const handleDetachProtection = async () => {
    if (!bindingId) return;
    const confirmed = window.confirm(
      '停用后将解除该 Agent 的敏感数据外发保护，发现风险时不再记录证据。\n\n确认停用保护吗？'
    );
    if (!confirmed) return;
    setDetaching(true);
    try {
      await onDetachProtection(agent.pid, bindingId);
    } finally {
      setDetaching(false);
    }
  };

  // Real Gateway = a service process that listens on a port itself (e.g. OpenClaw Gateway).
  // Promoted Gateway = a single-process agent promoted to a main card (e.g. Hermes Python CLI) —
  // these should not carry a "Gateway" label since they have no gateway concept.
  const hasPorts = (agent.ports?.length ?? 0) > 0;
  const isRealGateway = agent.role === 'gateway' && hasPorts;

  // Promoted Gateway + status=no_port uses the green "Running" label to avoid
  // the gray "client process" semantics conflicting with main-card identity.
  const useRunningStatus = isPromotedGatewayRunning(agent);
  const dotColor = useRunningStatus
    ? 'bg-green-500'
    : STATUS_COLORS[agent.status] || 'bg-gray-400';
  const labelKey = STATUS_LABEL_KEY[agent.status];
  const label = useRunningStatus
    ? t('ah.running')
    : labelKey ? t(labelKey) : agent.status;
  const tooltipKey = STATUS_TOOLTIP_KEY[agent.status];
  const tooltip = useRunningStatus
    ? t('ah.tooltip.running')
    : tooltipKey ? t(tooltipKey) : '';
  const isOffline = agent.status === 'offline';
  const isHung = agent.status === 'hung';
  const isUnhealthy = agent.status === 'unhealthy';
  const canRestart = isHung && !!agent.restart_cmd?.length;

  // Time until offline entries are auto-removed (5-minute TTL)
  const OFFLINE_TTL_MS = 5 * 60 * 1000;
  const offlineRemainSec =
    isOffline && agent.offline_since
      ? Math.max(0, Math.ceil((OFFLINE_TTL_MS - (Date.now() - agent.offline_since)) / 1000))
      : null;

  const borderClass = isHung
    ? 'border-orange-300 bg-orange-50'
    : isUnhealthy
    ? 'border-red-300 bg-red-50'
    : 'border-gray-200 bg-white';
  const nameColor = isOffline
    ? 'text-gray-500'
    : isHung
    ? 'text-orange-700'
    : isUnhealthy
    ? 'text-red-700'
    : 'text-gray-900';
  const labelColor = isOffline
    ? 'text-gray-400'
    : isHung
    ? 'text-orange-500 font-semibold'
    : isUnhealthy
    ? 'text-red-500 font-semibold'
    : 'text-gray-400';

  const remainText = offlineRemainSec !== null && offlineRemainSec > 0
    ? (offlineRemainSec >= 60
        ? `${Math.ceil(offlineRemainSec / 60)} ${t('common.min')}`
        : `${offlineRemainSec} ${t('common.sec')}`)
    : null;

  return (
    <div className={`rounded-lg border shadow-sm p-4 ${borderClass}`} title={tooltip}>
      <div className="flex items-center gap-2">
        <span className={`inline-block w-2.5 h-2.5 rounded-full flex-shrink-0 ${dotColor}`} />
        <span className={`font-medium text-sm truncate ${nameColor}`}>{agent.agent_name}</span>
        {isRealGateway && (
          <span className="text-[10px] px-1 py-0.5 rounded bg-green-100 text-green-700 font-medium">
            {t('ah.gateway')}
          </span>
        )}
        {agent.role === 'client' && (
          <span className="text-[10px] px-1 py-0.5 rounded bg-gray-100 text-gray-500 font-medium">
            {t('ah.client')}
          </span>
        )}
        {agent.role === 'worker' && (
          <span className="text-[10px] px-1 py-0.5 rounded bg-gray-100 text-gray-500 font-medium">
            {t('ah.worker')}
          </span>
        )}
        <span className={`ml-auto text-xs flex-shrink-0 ${labelColor}`}>{label}</span>
      </div>
      {tooltip && (
        <div className="mt-1.5 text-[11px] leading-snug text-gray-500 italic">ℹ️ {tooltip}</div>
      )}
      <div className="mt-2 text-xs text-gray-500 space-y-0.5">
        <div className="flex items-center gap-3">
          <span>PID {agent.pid}</span>
          {showProcessDetails && (
            <span className="text-gray-400">
              {agent.ports?.length ? agent.ports.map(port => `:${port}`).join(', ') : '—'}
            </span>
          )}
          {showProcessDetails ? (
            <span
              className={
                agent.latency_ms !== null && agent.status === 'healthy'
                  ? 'text-green-600'
                  : 'text-gray-400'
              }
            >
              {agent.latency_ms !== null ? `${agent.latency_ms}ms` : '—'}
            </span>
          ) : (
            agent.latency_ms !== null && agent.status === 'healthy' && (
              <span className="text-green-600">{agent.latency_ms}ms</span>
            )
          )}
          <span className="text-gray-400">{relativeTime(agent.last_check_time, t)}</span>
        </div>
        {agent.error_message && !isOffline && (
          <div
            className={`truncate ${isHung ? 'text-orange-500' : 'text-red-500'}`}
            title={agent.error_message}
          >
            {agent.error_message}
          </div>
        )}
        {isOffline && offlineRemainSec !== null && (
          <div className="text-gray-400 italic">
            {remainText
              ? t('ah.untilAutoRemoval', { time: remainText })
              : t('ah.removingSoon')}
          </div>
        )}
      </div>
      {latency && <LatencyMetricsRow metrics={latency} />}
      {!isOffline && (
        <div className="mt-3 pt-2 border-t border-gray-100 space-y-2.5">
          {/* 第一行：标题 + 当前状态描述  |  带文字标签的保护开关 */}
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0">
              <div className="text-xs font-medium text-gray-700">安全保护</div>
              <div className="text-[11px] text-gray-400 mt-0.5 leading-snug">
                {protectedByPolicy
                  ? (sources.length > 0
                      ? `审计模式 · 记录风险不阻断 · ${sources.length} 个敏感源`
                      : '审计模式 · 记录风险不阻断')
                  : '未开启 · 开启后进入审计模式，仅记录风险不阻断'}
              </div>
            </div>
            {/* 开关 + 状态文字：垂直堆叠，让用户一眼看懂开/关及其含义 */}
            <div className="flex flex-col items-end gap-1 flex-shrink-0">
              <button
                type="button"
                role="switch"
                aria-checked={protectedByPolicy}
                aria-label={protectedByPolicy ? '关闭 Agent 安全保护' : '开启 Agent 安全保护（进入审计模式）'}
                title={protectedByPolicy ? '点击关闭安全保护' : '点击开启安全保护：进入审计模式，发现风险时记录证据、不阻断 Agent'}
                disabled={protecting || detaching}
                onClick={() => {
                  if (protectedByPolicy) { void handleDetachProtection(); }
                  else { void enableProtection(); }
                }}
                onKeyDown={(e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); e.currentTarget.click(); } }}
                className={[
                  'relative inline-flex h-5 w-9 items-center rounded-full transition-colors duration-200 focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 focus-visible:ring-offset-1',
                  protectedByPolicy ? 'bg-blue-600' : 'bg-gray-300',
                  (protecting || detaching) ? 'opacity-50 cursor-not-allowed' : 'cursor-pointer',
                ].join(' ')}
              >
                <span
                  className={[
                    'inline-block h-3.5 w-3.5 rounded-full bg-white shadow-sm transform transition-transform duration-200',
                    protectedByPolicy ? 'translate-x-[18px]' : 'translate-x-[3px]',
                  ].join(' ')}
                />
                {(protecting || detaching) && (
                  <span className="absolute inset-0 flex items-center justify-center">
                    <span className="block h-2.5 w-2.5 rounded-full border-2 border-white border-t-transparent animate-spin" />
                  </span>
                )}
              </button>
              <span className={`text-[10px] leading-none font-medium ${protectedByPolicy ? 'text-blue-600' : 'text-gray-400'}`}>
                {protectedByPolicy ? '保护已开启' : '保护已关闭'}
              </span>
            </div>
          </div>

          {/* 第二行：待研判提醒（独占一行，仅开启且有待研判时显示，不再与开关重叠） */}
          {protectedByPolicy && pendingCaseCount > 0 && (
            <a href={`#/audit?agent_id=${encodeURIComponent(agent.agent_name)}`}
               className="inline-flex items-center gap-1 text-xs text-red-600 hover:underline">
              <span className="w-2 h-2 bg-red-500 rounded-full animate-pulse" />
              {pendingCaseCount} 件待研判
            </a>
          )}

          {/* 第三行：查看链接 + 保护设置（右对齐，避免与开关拥挤） */}
          <div className="flex items-center gap-3">
            {protectedByPolicy && (
              <>
                <a href={`#/audit?agent_id=${encodeURIComponent(agent.agent_name)}`}
                   className="text-xs text-blue-600 hover:underline">查看案件</a>
                <a href={`#/enforcement?agent_id=${encodeURIComponent(agent.agent_name)}`}
                   className="text-xs text-blue-600 hover:underline">查看拦截</a>
              </>
            )}
            <button
              onClick={openProtectionDialog}
              title="打开保护配置：设置保护目录、敏感文件与可信网络目标"
              className={`ml-auto text-xs ${protectedByPolicy ? 'text-blue-600 hover:text-blue-700' : 'text-gray-500 hover:text-gray-700'}`}
            >
              保护设置
            </button>
          </div>
        </div>
      )}
      {(isOffline || canRestart) && (
        <div className="mt-2 flex items-center gap-3">
          {isOffline && (
            <button
              onClick={() => onDelete(agent.pid)}
              className="text-xs text-gray-400 hover:text-gray-600 underline"
            >
              {t('ah.removeNow')}
            </button>
          )}
          {canRestart && (
            <button
              onClick={() => onRestart(agent.pid)}
              disabled={restarting}
              className="text-xs text-orange-500 hover:text-orange-700 underline disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {restarting ? t('ah.restarting') : t('ah.restartProcess')}
            </button>
          )}
        </div>
      )}
      {related.length > 0 && (
        <div className="mt-2 pt-2 border-t border-gray-100">
          <button
            onClick={() => setShowRelated(o => !o)}
            className="text-[11px] text-gray-500 hover:text-gray-700 flex items-center gap-1"
          >
            <span className={`transition-transform ${showRelated ? 'rotate-90' : ''}`}>▶</span>
            {t('ah.relatedProcesses', { n: related.length })}
          </button>
          {showRelated && (
            <div className="mt-1 ml-2 border-l-2 border-gray-200 pl-2 space-y-1">
              {related.map(ca => (
                <div key={ca.pid} className="text-[11px] text-gray-500 flex items-center gap-1.5">
                  <span className="inline-block w-1.5 h-1.5 rounded-full bg-gray-300" />
                  <span className="text-[10px] px-1 py-0.5 rounded bg-gray-100">
                    {ca.role === 'worker' ? t('ah.worker') : t('ah.client')}
                  </span>
                  <span className="text-gray-400">PID {ca.pid}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
      {showProtection && (
        <div className="fixed inset-0 z-50 bg-black/30 flex items-center justify-center p-4" onClick={() => setShowProtection(false)}>
          <div className="w-full max-w-lg rounded-xl bg-white shadow-xl border border-gray-200 p-5" onClick={event => event.stopPropagation()}>
            <div className="flex items-start justify-between gap-4">
              <div><h3 className="text-base font-semibold text-gray-900">Agent 安全保护</h3><p className="mt-1 text-xs text-gray-500">默认使用审计模式，不阻断任务；不会读取文件内容。Agent 重启后需重新开启。</p></div>
              <button onClick={() => setShowProtection(false)} className="text-gray-400 hover:text-gray-600">✕</button>
            </div>
            <label className="block mt-4 text-xs font-medium text-gray-700">保护目录</label>
            <div className="mt-1 flex gap-2"><input value={directory} onChange={event => setDirectory(event.target.value)} className="min-w-0 flex-1 rounded border border-gray-300 px-3 py-2 text-sm" placeholder="未获取到工作目录，请手动填写绝对路径后点扫描" /><button onClick={handleScan} className="rounded border border-gray-300 px-3 text-xs hover:bg-gray-50">扫描</button></div>
            <label className="block mt-4 text-xs font-medium text-gray-700">敏感文件（每行一个）</label>
            <textarea value={sources.join('\n')} onChange={event => setSources(event.target.value.split('\n').map(value => value.trim()).filter(Boolean))} rows={4} className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-xs font-mono" placeholder="扫描后自动填充 .env、credential、私钥等文件" />
            <label className="block mt-4 text-xs font-medium text-gray-700">可信网络目标（可选）</label>
            <input value={trustedTarget} onChange={event => setTrustedTarget(event.target.value)} className="mt-1 w-full rounded border border-gray-300 px-3 py-2 text-sm" placeholder="例如 10.0.0.8:443；当前运行时最多 1 个" />
            <div className="mt-5 flex justify-end gap-2"><button onClick={() => setShowProtection(false)} className="rounded border border-gray-300 px-4 py-2 text-sm">取消</button><button onClick={() => void applyProtection()} disabled={protecting || sources.length === 0} className="rounded bg-blue-600 px-4 py-2 text-sm text-white disabled:opacity-50">开启审计保护</button></div>
          </div>
        </div>
      )}
    </div>
  );
};

const AgentGroupCard: React.FC<{
  group: AgentGroup;
  clientAgents: AgentHealthStatus[];
  onDelete: (pid: number) => void;
  onRestart: (pid: number) => void;
  restartingPids: Set<number>;
  protectionBindings?: Map<number, string>;
  onProtected?: (pid: number, bindingId: string) => void;
  onDetachProtection?: (pid: number, bindingId: string) => Promise<void> | void;
  addToast?: (message: string) => void;
  pendingCaseCounts?: Map<string, number>;
  latency?: LatencyMetricsSummary;
}> = ({ group, clientAgents, onDelete, onRestart, restartingPids, protectionBindings, onProtected, onDetachProtection, addToast, pendingCaseCounts, latency }) => {
  const { t } = useI18n();
  const isHung = group.status === 'hung';
  const isUnhealthy = group.status === 'unhealthy';
  const isOffline = group.status === 'offline';
  const hasAbnormalProcess = isHung || isUnhealthy;
  const [showProcesses, setShowProcesses] = useState(hasAbnormalProcess);

  useEffect(() => {
    if (hasAbnormalProcess) {
      setShowProcesses(true);
    }
  }, [hasAbnormalProcess]);

  const useRunningStatus = group.agents.every(isPromotedGatewayRunning);
  const dotColor = useRunningStatus
    ? 'bg-green-500'
    : STATUS_COLORS[group.status] || 'bg-gray-400';
  const labelKey = STATUS_LABEL_KEY[group.status];
  const label = useRunningStatus
    ? t('ah.running')
    : labelKey ? t(labelKey) : group.status;
  const tooltipKey = STATUS_TOOLTIP_KEY[group.status];
  const tooltip = useRunningStatus
    ? t('ah.tooltip.running')
    : tooltipKey ? t(tooltipKey) : '';
  const borderClass = isHung
    ? 'border-orange-300 bg-orange-50'
    : isUnhealthy
    ? 'border-red-300 bg-red-50'
    : 'border-gray-200 bg-white';
  const nameColor = isOffline
    ? 'text-gray-500'
    : isHung
    ? 'text-orange-700'
    : isUnhealthy
    ? 'text-red-700'
    : 'text-gray-900';
  const labelColor = isOffline
    ? 'text-gray-400'
    : isHung
    ? 'text-orange-500 font-semibold'
    : isUnhealthy
    ? 'text-red-500 font-semibold'
    : 'text-gray-400';

  return (
    <div className={`rounded-lg border shadow-sm p-4 ${borderClass}`} title={tooltip}>
      <div className="flex items-center gap-2">
        <span
          className={`inline-block w-2.5 h-2.5 rounded-full flex-shrink-0 ${dotColor}`}
        />
        <span className={`font-medium text-sm truncate ${nameColor}`}>{group.agentName}</span>
        <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-gray-100 text-gray-600 font-semibold">
          ×{group.agents.length}
        </span>
        <span className={`ml-auto text-xs flex-shrink-0 ${labelColor}`}>{label}</span>
      </div>
      {tooltip && (
        <div className="mt-1.5 text-[11px] leading-snug text-gray-500 italic">ℹ️ {tooltip}</div>
      )}
      {latency && <LatencyMetricsRow metrics={latency} />}
      <div className="mt-2 pt-2 border-t border-gray-100">
        <button
          onClick={() => setShowProcesses(open => !open)}
          className="text-[11px] text-gray-500 hover:text-gray-700 flex items-center gap-1"
        >
          <span className={`transition-transform ${showProcesses ? 'rotate-90' : ''}`}>▶</span>
          {t('ah.processList', { n: group.agents.length })}
        </button>
        {showProcesses && (
          <div className="mt-2 space-y-2">
            {group.agents.map(agent => (
              <AgentCard
                key={agent.pid}
                agent={agent}
                related={clientAgents.filter(client => client.parent_pid === agent.pid)}
                onDelete={onDelete}
                onRestart={onRestart}
                restarting={restartingPids.has(agent.pid)}
                protectedByPolicy={!!protectionBindings?.has(agent.pid)}
                bindingId={protectionBindings?.get(agent.pid)}
                onProtected={onProtected}
                onDetachProtection={onDetachProtection}
                addToast={addToast}
                pendingCaseCount={pendingCaseCounts?.get(agent.agent_name) ?? 0}
                showProcessDetails
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );
};

const AgentStatusSection: React.FC<{ addToast: (msg: string) => void }> = ({ addToast }) => {
  const { t } = useI18n();
  const [agents, setAgents] = useState<AgentHealthStatus[]>([]);
  const [clientAgents, setClientAgents] = useState<AgentHealthStatus[]>([]);
  const [showOrphans, setShowOrphans] = useState(false);
  const [lastScan, setLastScan] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [restartingPids, setRestartingPids] = useState<Set<number>>(new Set());
  const [protectionBindings, setProtectionBindings] = useState<Map<number, string>>(new Map());
  // agent_id → 待研判（status==='open'）案件数，用于 AgentCard badge
  const [pendingCasesByAgent, setPendingCasesByAgent] = useState<Map<string, number>>(new Map());
  const hasDataRef = useRef(false);
  const [rangeMs, setRangeMs] = useState(7 * 24 * 3600 * 1000);
  const [latencyMetrics, setLatencyMetrics] = useState<LatencyMetricsSummary[]>([]);
  const [latencyLoading, setLatencyLoading] = useState(true);
  const [latencyError, setLatencyError] = useState<string | null>(null);
  const latencyRequestIdRef = useRef(0);

  const loadLatency = useCallback(async () => {
    const requestId = ++latencyRequestIdRef.current;
    setLatencyLoading(true);
    setLatencyError(null);
    try {
      const endNs = Date.now() * 1_000_000;
      const startNs = endNs - rangeMs * 1_000_000;
      const data = await fetchLatencyMetrics(startNs, endNs);
      if (requestId === latencyRequestIdRef.current) {
        setLatencyMetrics(data);
        setLatencyError(null);
      }
    } catch (e: any) {
      if (requestId === latencyRequestIdRef.current) {
        setLatencyError(e.message || '');
      }
    } finally {
      if (requestId === latencyRequestIdRef.current) {
        setLatencyLoading(false);
      }
    }
  }, [rangeMs]);

  const refresh = useCallback(async () => {
    try {
      // Fetch all at once (including client/worker), then group by parent_pid under each main card
      const data = await fetchAgentProcessHealth({ includeClients: true });
      const agentRows = Array.isArray(data?.agents) ? data.agents : [];
      setAgents(agentRows.filter(a => a.role === 'gateway'));
      setClientAgents(agentRows.filter(a => a.role !== 'gateway'));
      setLastScan(data.last_scan_time ?? 0);
      void fetchEnforcementBindings().then(({ bindings }) => {
        // 活跃 binding = pending/enforced/degraded；建立 root_pid → binding_id 映射用于停用保护
        const nextBindings = new Map<number, string>();
        for (const binding of bindings) {
          if (binding.state !== 'pending' && binding.state !== 'enforced' && binding.state !== 'degraded') continue;
          // Only track credential-protection bindings on Agent cards
          if (binding.request.policy_id !== CREDENTIAL_POLICY_ID) continue;
          const pid = binding.request.root_pid;
          // enforced 优先于 pending/degraded，避免同 pid 多 binding 时取错
          if (!nextBindings.has(pid) || binding.state === 'enforced') {
            nextBindings.set(pid, binding.request.binding_id);
          }
        }
        setProtectionBindings(nextBindings);
      }).catch(() => undefined);
      // 同一加载流程内附带拉取风险案件，按 agent_id 分组统计待研判（open）数量
      void fetchSecurityCases({ limit: 500 }).then((response) => {
        const counts = new Map<string, number>();
        for (const riskCase of response.data.items) {
          if (riskCase.status !== 'open') continue;
          counts.set(riskCase.agent_id, (counts.get(riskCase.agent_id) ?? 0) + 1);
        }
        setPendingCasesByAgent(counts);
      }).catch(() => undefined);
      setError(null);
      hasDataRef.current = true;
    } catch (e: any) {
      if (!hasDataRef.current) {
        setError(e.message || t('ah.requestFailed'));
      }
    } finally {
      setLoading(false);
    }
  }, [t]);

  const handleDelete = async (pid: number) => {
    try {
      await deleteAgentHealth(pid);
      setAgents(prev => prev.filter(a => a.pid !== pid));
    } catch (e: any) {
      addToast(t('ah.deleteFailed', { msg: e.message }));
    }
  };

  const handleRestart = async (pid: number) => {
    setRestartingPids(prev => new Set(prev).add(pid));
    try {
      const result = await restartAgentHealth(pid);
      addToast(t('ah.restartSucceeded', { pid: result.new_pid }));
      // Remove the old entry from the local list immediately (the new PID appears within 30s)
      setAgents(prev => prev.filter(a => a.pid !== pid));
    } catch (e: any) {
      addToast(t('ah.restartFailed', { msg: e.message }));
    } finally {
      setRestartingPids(prev => {
        const next = new Set(prev);
        next.delete(pid);
        return next;
      });
    }
  };

  const handleDetachProtection = useCallback(async (pid: number, bindingId: string) => {
    try {
      await detachEnforcementBinding(bindingId);
      setProtectionBindings(prev => {
        const next = new Map(prev);
        next.delete(pid);
        return next;
      });
      addToast('✅ 已停用保护');
      void refresh();
    } catch (e: any) {
      addToast(`停用失败: ${e.message ?? '请稍后重试'}`);
    }
  }, [addToast, refresh]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(refresh, 10_000);
    return () => clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    void loadLatency();
  }, [loadLatency]);

  const groupedAgents = useMemo(
    () =>
      groupAgentsByName(agents).sort(
        (a, b) => CARD_STATUS_ORDER[a.status] - CARD_STATUS_ORDER[b.status],
    ),
    [agents],
  );

  // Count promoted gateways (no_port but running) as healthy so the summary
  // ratio matches the green "Running" rendering of their cards.
  const healthyCount = agents.filter(
    a => a.status === 'healthy' || isPromotedGatewayRunning(a)
  ).length;
  const offlineCount = agents.filter(a => a.status === 'offline').length;
  const hungCount = agents.filter(a => a.status === 'hung').length;
  const totalCount = agents.length;

  const latencyByAgent = new Map<string, LatencyMetricsSummary[]>();
  for (const metric of latencyMetrics) {
    if (metric.agent_name !== null) {
      const key = canonicalAgentKey(metric.agent_name);
      const summaries = latencyByAgent.get(key);
      if (summaries) {
        summaries.push(metric);
      } else {
        latencyByAgent.set(key, [metric]);
      }
    }
  }
  const latencyForAgent = (agentName: string): LatencyMetricsSummary | undefined => {
    const summaries = latencyByAgent.get(canonicalAgentKey(agentName));
    // Do not silently choose one when casing variants produce separate summaries.
    return summaries?.length === 1 ? summaries[0] : undefined;
  };

  const gatewayPids = new Set(agents.map(a => a.pid));
  // Orphaned related processes: workers whose parent is not any main card (should not happen, fallback).
  // Filter out status=offline processes — they are TTL-cleaned after 5 minutes.
  const orphans = clientAgents.filter(
    c =>
      c.status !== 'offline' &&
      (c.parent_pid === undefined || c.parent_pid === null || !gatewayPids.has(c.parent_pid))
  );

  return (
    <section>
      <div className="flex items-center justify-between mb-3">
        <div className="flex items-center gap-3">
          <h2
            className="text-lg font-semibold text-gray-800 cursor-help"
            title={t('ah.runtimeHealthTooltip')}
          >
            {t('ah.runtimeHealth')}
          </h2>
          {offlineCount > 0 && (
            <span className="text-xs px-1.5 py-0.5 rounded-full bg-red-100 text-red-600 font-semibold">
              {t('ah.crashed', { n: offlineCount })}
            </span>
          )}
          {hungCount > 0 && (
            <span className="text-xs px-1.5 py-0.5 rounded-full bg-orange-100 text-orange-600 font-semibold">
              {t('ah.hungCount', { n: hungCount })}
            </span>
          )}
          {totalCount > 0 && (
            <span className="text-xs px-1.5 py-0.5 rounded-full bg-gray-100 text-gray-600">
              {t('ah.healthyRatio', { healthy: healthyCount, total: totalCount })}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2 flex-wrap justify-end">
          <div className="flex items-center gap-1.5">
            <span className="text-[11px] text-gray-400">{t('latency.title')}</span>
            {LATENCY_TIME_PRESETS.map(({ key, ms }) => (
              <button
                key={key}
                type="button"
                onClick={() => setRangeMs(ms)}
                aria-pressed={rangeMs === ms}
                className={rangeMs === ms
                  ? 'px-2 py-1 text-[11px] rounded bg-blue-100 text-blue-700 font-medium'
                  : 'px-2 py-1 text-[11px] rounded bg-gray-100 hover:bg-gray-200 text-gray-600'}
              >
                {t(key)}
              </button>
            ))}
          </div>
          {latencyLoading && (
            <span className="text-[11px] text-gray-400">{t('latency.loading')}</span>
          )}
          {latencyError !== null && (
            <span className="text-[11px] text-red-400" title={latencyError || t('latency.error')}>
              {t('latency.error')}
            </span>
          )}
          {lastScan > 0 && (
            <span className="text-xs text-gray-400">{t('ah.lastScan', { time: relativeTime(lastScan, t) })}</span>
          )}
        </div>
      </div>

      {loading ? (
        <div className="py-8 text-center text-sm text-gray-400">{t('common.loading')}</div>
      ) : error ? (
        <div className="py-8 text-center text-sm text-red-400">{error}</div>
      ) : groupedAgents.length === 0 ? (
        <div className="py-8 text-center text-sm text-gray-400 bg-white rounded-lg border border-gray-200">
          {t('ah.noAgents')}
        </div>
      ) : (
        <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-3">
          {groupedAgents.map(group =>
            group.agents.length === 1 ? (
              <AgentCard
                key={group.agents[0].pid}
                agent={group.agents[0]}
                // 只把 parent_pid 与当前主卡 pid 严格匹配的进程挂为关联进程，
                // 避免同名独立实例（两个独立终端各开一个 hermes）被错误合并。
                related={clientAgents.filter(c => c.parent_pid === group.agents[0].pid)}
                onDelete={handleDelete}
                onRestart={handleRestart}
                restarting={restartingPids.has(group.agents[0].pid)}
                protectedByPolicy={protectionBindings.has(group.agents[0].pid)}
                bindingId={protectionBindings.get(group.agents[0].pid)}
                pendingCaseCount={pendingCasesByAgent.get(group.agents[0].agent_name) ?? 0}
                onProtected={(pid, bindingId) => setProtectionBindings(previous => new Map(previous).set(pid, bindingId))}
                onDetachProtection={handleDetachProtection}
                addToast={addToast}
                latency={latencyForAgent(group.agentName)}
              />
            ) : (
              <AgentGroupCard
                key={group.agentName}
                group={group}
                clientAgents={clientAgents}
                onDelete={handleDelete}
                onRestart={handleRestart}
                restartingPids={restartingPids}
                protectionBindings={protectionBindings}
                onProtected={(pid, bindingId) => setProtectionBindings(previous => new Map(previous).set(pid, bindingId))}
                onDetachProtection={handleDetachProtection}
                addToast={addToast}
                pendingCaseCounts={pendingCasesByAgent}
                latency={latencyForAgent(group.agentName)}
              />
            ),
          )}
        </div>
      )}

      {orphans.length > 0 && (
        <div className="mt-3">
          <button
            onClick={() => setShowOrphans(s => !s)}
            className="text-xs text-gray-500 hover:text-gray-700 flex items-center gap-1"
          >
            <span className={`transition-transform ${showOrphans ? 'rotate-90' : ''}`}>▶</span>
            {t('ah.orphanedProcesses', { n: orphans.length })}
          </button>
          {showOrphans && (
            <div className="mt-1 ml-2 border-l-2 border-gray-100 pl-2 space-y-1">
              {orphans.map(ca => (
                <div key={ca.pid} className="text-[11px] text-gray-500 flex items-center gap-1.5">
                  <span className="inline-block w-1.5 h-1.5 rounded-full bg-gray-300" />
                  <span className="font-medium">{ca.agent_name}</span>
                  <span className="text-[10px] px-1 py-0.5 rounded bg-gray-100">
                    {ca.role === 'worker' ? t('ah.worker') : t('ah.client')}
                  </span>
                  <span className="text-gray-400">PID {ca.pid}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </section>
  );
};

// ─── Interruption events section ──────────────────────────────────────────────

const SEVERITY_DOT: Record<InterruptionSeverity, string> = {
  critical: 'bg-red-600',
  high: 'bg-orange-500',
  medium: 'bg-yellow-400',
  low: 'bg-blue-400',
};

const SEVERITY_LABEL_KEY: Record<InterruptionSeverity, MessageKey> = {
  critical: 'common.critical',
  high: 'common.high',
  medium: 'common.medium',
  low: 'common.low',
};

const TIME_RANGE_KEYS: { labelKey: MessageKey; hours: number }[] = [
  { labelKey: 'ah.last1Hour', hours: 1 },
  { labelKey: 'ah.last24Hours', hours: 24 },
  { labelKey: 'ah.last7Days', hours: 24 * 7 },
];

function parseDetail(raw: string | null, t: (key: MessageKey) => string): React.ReactNode {
  if (!raw) return <span className="text-xs text-gray-400">{t('ah.noDetails')}</span>;
  try {
    const obj = JSON.parse(raw);
    return (
      <pre className="text-xs bg-gray-100 text-gray-700 rounded p-2 whitespace-pre-wrap break-all border border-gray-200">
        {JSON.stringify(obj, null, 2)}
      </pre>
    );
  } catch {
    return <span className="text-xs text-gray-500">{raw}</span>;
  }
}

/** Truncated monospace ID with a small copy button. */
const IDWithCopy: React.FC<{ value: string | null; addToast: (msg: string) => void }> = ({
  value,
  addToast,
}) => {
  const { t } = useI18n();
  if (!value) return <span className="text-gray-400">—</span>;
  const short = value.length > 8 ? `${value.slice(0, 8)}…` : value;
  const copy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    let ok = false;
    try {
      // Prefer the clipboard API (HTTPS / localhost)
      if (window.isSecureContext && navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(value);
        ok = true;
      }
    } catch {
      // fall through
    }
    if (!ok) {
      // Fallback: temporary textarea + execCommand, works over HTTP too
      try {
        const ta = document.createElement('textarea');
        ta.value = value;
        ta.setAttribute('readonly', '');
        ta.style.position = 'fixed';
        ta.style.left = '-9999px';
        ta.style.opacity = '0';
        document.body.appendChild(ta);
        ta.select();
        ok = document.execCommand('copy');
        document.body.removeChild(ta);
      } catch {
        ok = false;
      }
    }
    addToast(ok ? t('ah.copiedValue', { value }) : t('ah.copyFailedValue', { value }));
  };
  return (
    <span className="inline-flex items-center gap-1 font-mono text-xs text-gray-600" title={value}>
      <span>{short}</span>
      <button
        onClick={copy}
        type="button"
        aria-label={t('common.copy')}
        className="text-gray-400 hover:text-blue-600 p-0.5 rounded hover:bg-blue-50"
        title={t('common.copyFullId')}
      >
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 16 16"
          fill="currentColor"
          className="w-3 h-3"
        >
          <path d="M4 4v-1a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-1v1a2 2 0 0 1-2 2h-5a2 2 0 0 1-2-2v-5a2 2 0 0 1 2-2z" />
        </svg>
      </button>
    </span>
  );
};

const InterruptionEventRow: React.FC<{
  event: InterruptionRecord;
  onResolved: (id: string) => void;
  addToast: (msg: string) => void;
}> = ({ event, onResolved, addToast }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [expanded, setExpanded] = useState(false);
  const [resolving, setResolving] = useState(false);

  const dotStyle = SEVERITY_DOT[event.severity] ?? 'bg-gray-400';
  const itypeKey = interruptionTypeKey(event.interruption_type);
  const typeLabel = itypeKey ? t(itypeKey) : event.interruption_type;
  const sevKey = SEVERITY_LABEL_KEY[event.severity];

  const handleResolve = async () => {
    const confirmed = window.confirm(t('ah.markResolvedConfirm'));
    if (!confirmed) return;
    setResolving(true);
    try {
      await resolveInterruption(event.interruption_id);
      onResolved(event.interruption_id);
    } catch (e: any) {
      addToast(t('ah.markFailed', { msg: e.message ?? '' }));
    } finally {
      setResolving(false);
    }
  };

  return (
    <>
      <tr className={`border-t border-gray-100 ${event.resolved ? 'opacity-60' : ''}`}>
        <td className="px-3 py-2 whitespace-nowrap text-xs text-gray-500">
          {formatNs(event.occurred_at_ns, locale)}
        </td>
        <td className="px-3 py-2 text-xs text-gray-700 truncate max-w-[10rem]">
          {event.agent_name ?? '—'}
        </td>
        <td className="px-3 py-2 text-xs text-gray-800 whitespace-nowrap">{typeLabel}</td>
        <td className="px-3 py-2 whitespace-nowrap">
          <span className="inline-flex items-center gap-1.5 text-xs text-gray-700">
            <span className={`inline-block w-2 h-2 rounded-full ${dotStyle}`} />
            {sevKey ? t(sevKey) : event.severity}
          </span>
        </td>
        <td className="px-3 py-2">
          <IDWithCopy value={event.session_id} addToast={addToast} />
        </td>
        <td className="px-3 py-2">
          <IDWithCopy value={event.conversation_id} addToast={addToast} />
        </td>
        <td className="px-3 py-2 whitespace-nowrap text-xs">
          {event.resolved ? (
            <span className="text-green-600">{t('common.resolved')}</span>
          ) : (
            <span className="text-red-500">{t('common.unresolved')}</span>
          )}
        </td>
        <td className="px-3 py-2 whitespace-nowrap text-right">
          <div className="flex items-center gap-1 justify-end">
            {!event.resolved && (
              <button
                onClick={handleResolve}
                disabled={resolving}
                title={t('ah.markResolvedTitle')}
                className="text-xs px-2 py-0.5 rounded bg-green-600 hover:bg-green-500 text-white disabled:opacity-50"
              >
                {resolving ? '…' : t('common.resolve')}
              </button>
            )}
            <button
              onClick={() => setExpanded(x => !x)}
              className="text-xs px-2 py-0.5 rounded border border-gray-300 text-gray-600 hover:bg-gray-50"
            >
              {expanded ? t('common.collapse') : t('common.details')}
            </button>
          </div>
        </td>
      </tr>
      {expanded && (
        <tr className="border-t border-gray-50 bg-gray-50/50">
          <td colSpan={8} className="px-3 py-2">
            {event.call_id && (
              <div className="mb-1 text-xs text-gray-400">{t('ah.callLabel', { id: event.call_id })}</div>
            )}
            {parseDetail(event.detail, t)}
          </td>
        </tr>
      )}
    </>
  );
};

const PAGE_SIZES = [15, 30, 50];

const InterruptionSection: React.FC<{ addToast: (msg: string) => void }> = ({ addToast }) => {
  const { t } = useI18n();
  const [events, setEvents] = useState<InterruptionRecord[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [hours, setHours] = useState(24);
  const [typeFilter, setTypeFilter] = useState('');
  const [severityFilter, setSeverityFilter] = useState('');
  const [unresolvedOnly, setUnresolvedOnly] = useState(true);
  const [page, setPage] = useState(1);
  const [pageSize, setPageSize] = useState(15);
  const hasDataRef = useRef(false);

  const load = useCallback(async () => {
    try {
      const endNs = Date.now() * 1_000_000;
      const startNs = endNs - hours * 3600 * 1_000_000_000;
      const data = await fetchInterruptions({
        startNs,
        endNs,
        interruptionType: typeFilter || undefined,
        severity: severityFilter || undefined,
        resolved: unresolvedOnly ? false : undefined,
        limit: 200,
      });
      setEvents(data);
      setError(null);
      hasDataRef.current = true;
    } catch (e: any) {
      if (!hasDataRef.current) {
        setError(e.message ?? t('ah.failedToLoad'));
      }
    } finally {
      setLoading(false);
    }
  }, [hours, typeFilter, severityFilter, unresolvedOnly, t]);

  useEffect(() => {
    setLoading(true);
    void load();
    const timer = setInterval(load, 30_000);
    return () => clearInterval(timer);
  }, [load]);

  // Reset to page 1 when filters change to avoid staying on an empty page
  useEffect(() => {
    setPage(1);
  }, [hours, typeFilter, severityFilter, unresolvedOnly]);

  const handleResolved = (id: string) => {
    if (unresolvedOnly) {
      setEvents(prev => prev.filter(e => e.interruption_id !== id));
    } else {
      setEvents(prev =>
        prev.map(e => (e.interruption_id === id ? { ...e, resolved: true } : e))
      );
    }
  };

  const unresolvedCount = events.filter(e => !e.resolved).length;

  const selectClass =
    'text-xs border border-gray-300 rounded px-2 py-1 bg-white text-gray-700 focus:outline-none focus:ring-1 focus:ring-blue-400';

  return (
    <section className="mt-8">
      <div className="flex items-center justify-between mb-3 flex-wrap gap-2">
        <div className="flex items-center gap-3">
          <h2 className="text-lg font-semibold text-gray-800">{t('ah.interruptionEvents')}</h2>
          {!loading && (
            <span className="text-xs text-gray-400">{t('ah.unresolvedCount', { n: unresolvedCount })}</span>
          )}
        </div>
        <div className="flex items-center gap-2 flex-wrap">
          <select value={hours} onChange={e => setHours(Number(e.target.value))} className={selectClass}>
            {TIME_RANGE_KEYS.map(r => (
              <option key={r.hours} value={r.hours}>
                {t(r.labelKey)}
              </option>
            ))}
          </select>
          <select value={typeFilter} onChange={e => setTypeFilter(e.target.value)} className={selectClass}>
            <option value="">{t('common.allTypes')}</option>
            {INTERRUPTION_TYPES.map(k => {
              const key = interruptionTypeKey(k);
              return (
                <option key={k} value={k}>
                  {key ? t(key) : k}
                </option>
              );
            })}
          </select>
          <select
            value={severityFilter}
            onChange={e => setSeverityFilter(e.target.value)}
            className={selectClass}
          >
            <option value="">{t('common.allSeverities')}</option>
            {(Object.keys(SEVERITY_LABEL_KEY) as InterruptionSeverity[]).map(s => (
              <option key={s} value={s}>
                {t(SEVERITY_LABEL_KEY[s])}
              </option>
            ))}
          </select>
          <label className="flex items-center gap-1 text-xs text-gray-600 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={unresolvedOnly}
              onChange={e => setUnresolvedOnly(e.target.checked)}
            />
            {t('common.unresolvedOnly')}
          </label>
        </div>
      </div>

      <div className="bg-white rounded-lg border border-gray-200 overflow-x-auto">
        {loading ? (
          <div className="py-8 text-center text-sm text-gray-400 animate-pulse">{t('common.loading')}</div>
        ) : error ? (
          <div className="py-8 text-center text-sm text-red-400">{error}</div>
        ) : events.length === 0 ? (
          <div className="py-8 text-center text-sm text-gray-400">{t('ah.noInterruptionEvents')}</div>
        ) : (
          <>
            <table className="w-full text-left">
              <thead>
                <tr className="text-xs text-gray-400">
                  <th className="px-3 py-2 font-medium">{t('common.time')}</th>
                  <th className="px-3 py-2 font-medium">{t('common.agent')}</th>
                  <th className="px-3 py-2 font-medium">{t('common.type')}</th>
                  <th className="px-3 py-2 font-medium">{t('common.severity')}</th>
                  <th className="px-3 py-2 font-medium">{t('common.session')}</th>
                  <th className="px-3 py-2 font-medium">{t('common.conversation')}</th>
                  <th className="px-3 py-2 font-medium">{t('common.status')}</th>
                  <th className="px-3 py-2 font-medium text-right">{t('common.actions')}</th>
                </tr>
              </thead>
              <tbody>
                {(() => {
                  const totalPages = Math.max(1, Math.ceil(events.length / pageSize));
                  const cur = Math.min(page, totalPages);
                  return events.slice((cur - 1) * pageSize, cur * pageSize).map(e => (
                    <InterruptionEventRow
                      key={e.interruption_id}
                      event={e}
                      onResolved={handleResolved}
                      addToast={addToast}
                    />
                  ));
                })()}
              </tbody>
            </table>
            {(() => {
              const totalPages = Math.max(1, Math.ceil(events.length / pageSize));
              const clamped = Math.min(page, totalPages);
              return (
                <div className="flex items-center justify-between px-3 py-2 border-t border-gray-200 text-xs text-gray-600">
                  <div className="flex items-center gap-2">
                    <span>{t('common.events', { n: events.length })}</span>
                    <span className="text-gray-400">·</span>
                    <span>
                      {t('common.page', { cur: clamped, total: totalPages })}
                    </span>
                    <span className="text-gray-400">·</span>
                    <label className="flex items-center gap-1">
                      {t('common.perPage')}
                      <select
                        value={pageSize}
                        onChange={e => {
                          setPageSize(Number(e.target.value));
                          setPage(1);
                        }}
                        className="text-xs border border-gray-300 rounded px-1 py-0.5 bg-white"
                      >
                        {PAGE_SIZES.map(s => (
                          <option key={s} value={s}>
                            {s}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>
                  <div className="flex items-center gap-1">
                    <button
                      onClick={() => setPage(1)}
                      disabled={clamped <= 1}
                      className="px-2 py-1 rounded border border-gray-300 disabled:opacity-40 disabled:cursor-not-allowed hover:bg-gray-50"
                    >
                      {t('common.first')}
                    </button>
                    <button
                      onClick={() => setPage(p => Math.max(1, p - 1))}
                      disabled={clamped <= 1}
                      className="px-2 py-1 rounded border border-gray-300 disabled:opacity-40 disabled:cursor-not-allowed hover:bg-gray-50"
                    >
                      {t('common.prev')}
                    </button>
                    <button
                      onClick={() => setPage(p => Math.min(totalPages, p + 1))}
                      disabled={clamped >= totalPages}
                      className="px-2 py-1 rounded border border-gray-300 disabled:opacity-40 disabled:cursor-not-allowed hover:bg-gray-50"
                    >
                      {t('common.next')}
                    </button>
                    <button
                      onClick={() => setPage(totalPages)}
                      disabled={clamped >= totalPages}
                      className="px-2 py-1 rounded border border-gray-300 disabled:opacity-40 disabled:cursor-not-allowed hover:bg-gray-50"
                    >
                      {t('common.last')}
                    </button>
                  </div>
                </div>
              );
            })()}
          </>
        )}
      </div>
    </section>
  );
};

// ─── Page ─────────────────────────────────────────────────────────────────────

export const AgentHealthPage: React.FC = () => {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastIdRef = useRef(0);

  const addToast = useCallback((message: string) => {
    const id = ++toastIdRef.current;
    setToasts(prev => [...prev, { id, message }]);
    setTimeout(() => setToasts(prev => prev.filter(t => t.id !== id)), 5000);
  }, []);

  return (
    <div className="max-w-screen-xl mx-auto px-6 py-6">
      <div className="fixed top-4 left-1/2 -translate-x-1/2 z-50 flex flex-col items-center gap-2 pointer-events-none">
        {toasts.map(t => (
          <div
            key={t.id}
            className="bg-gray-800 text-white text-sm px-5 py-3 rounded-lg shadow-xl pointer-events-auto"
          >
            {t.message}
          </div>
        ))}
      </div>

      <AgentActivitySection />
      <div className="mt-8">
        <AgentStatusSection addToast={addToast} />
      </div>
      <InterruptionSection addToast={addToast} />
    </div>
  );
};

export default AgentHealthPage;
