import { SecurityApiClientError } from '../../utils/apiClient';
import { formatMsCompact } from '../../utils/datetime';
import type { MessageKey } from '../../i18n';
import type {
  SecurityCountItem,
  SecurityEventRecord,
  SecurityTimelineItem,
  SecurityTimelineObservabilityContext,
} from '../../utils/apiClient';

export function msToNs(ms: number): number {
  return ms * 1_000_000;
}

export function fmtNumber(value: number | null | undefined): string {
  return (value ?? 0).toLocaleString();
}

export function shortId(value: string | null | undefined, len = 14): string {
  if (!value) return '-';
  return value.length > len ? `${value.slice(0, len)}...` : value;
}

export function timestampToMs(input: {
  timestamp_ns?: number | null;
  timestamp_epoch?: number | null;
  timestamp?: string | null;
  started_at_ns?: number | null;
  started_at_epoch?: number | null;
  first_seen_ns?: number | null;
  first_seen_epoch?: number | null;
}): number | null {
  if (typeof input.timestamp_ns === 'number') return input.timestamp_ns / 1_000_000;
  if (typeof input.timestamp_epoch === 'number') return input.timestamp_epoch * 1_000;
  if (typeof input.started_at_ns === 'number') return input.started_at_ns / 1_000_000;
  if (typeof input.started_at_epoch === 'number') return input.started_at_epoch * 1_000;
  if (typeof input.first_seen_ns === 'number') return input.first_seen_ns / 1_000_000;
  if (typeof input.first_seen_epoch === 'number') return input.first_seen_epoch * 1_000;
  if (input.timestamp) {
    const parsed = Date.parse(input.timestamp);
    return Number.isNaN(parsed) ? null : parsed;
  }
  return null;
}

export function fmtTime(input: Parameters<typeof timestampToMs>[0], locale: string): string {
  const ms = timestampToMs(input);
  if (ms == null) return '-';
  return formatMsCompact(ms, locale);
}

export function errorMessage(
  error: unknown,
  t?: (key: MessageKey) => string,
): string {
  if (error instanceof SecurityApiClientError) {
    return `${error.code}: ${error.message}`;
  }
  if (error instanceof Error) return error.message;
  return t ? t('sec.apiRequestFailed') : 'Security observability API request failed';
}

export function mapToCountItems(map: Record<string, number> | undefined): SecurityCountItem[] {
  return Object.entries(map ?? {})
    .map(([value, count]) => ({ value, count }))
    .sort((a, b) => b.count - a.count || String(a.value).localeCompare(String(b.value)));
}

export function recordPreview(value: unknown): string {
  if (value === null || value === undefined) return '-';
  if (typeof value === 'string') return value;
  if (typeof value === 'number' || typeof value === 'boolean') return String(value);
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

export function timelineKey(value: unknown): string | null {
  if (typeof value === 'string' || typeof value === 'number') return String(value);
  return null;
}

export function buildObservabilityContextById(items: SecurityTimelineItem[]): Map<string, SecurityTimelineItem> {
  const byId = new Map<string, SecurityTimelineItem>();
  for (const item of items) {
    const key = item.kind === 'observability' ? timelineKey(item.id) : null;
    if (key) byId.set(key, item);
  }
  return byId;
}

export function timelineObservabilityContext(
  item: SecurityTimelineItem,
  observabilityItemsById: Map<string, SecurityTimelineItem>,
): SecurityTimelineObservabilityContext {
  const nested = isRecord(item.observability)
    ? item.observability as SecurityTimelineObservabilityContext
    : undefined;
  const linkedKey = timelineKey(item.observability_event_id);
  const linked = linkedKey ? observabilityItemsById.get(linkedKey) : undefined;
  return {
    id: item.observability_event_id ?? nested?.id ?? linked?.id,
    hook: item.hook ?? nested?.hook ?? linked?.hook ?? null,
    timestamp: nested?.timestamp ?? linked?.timestamp ?? null,
    timestamp_epoch: nested?.timestamp_epoch ?? linked?.timestamp_epoch ?? null,
    session_id: item.session_id ?? nested?.session_id ?? linked?.session_id ?? null,
    run_id: item.run_id ?? nested?.run_id ?? linked?.run_id ?? null,
    call_id: item.call_id ?? nested?.call_id ?? linked?.call_id ?? null,
    tool_call_id: item.tool_call_id ?? nested?.tool_call_id ?? linked?.tool_call_id ?? null,
    metadata: item.metadata ?? nested?.metadata ?? linked?.metadata,
    metrics: item.metrics ?? nested?.metrics ?? linked?.metrics,
  };
}

const SECURITY_DETAIL_FIELDS: Array<{ id: string; labelKey: MessageKey; keys: string[] }> = [
  { id: 'verdict', labelKey: 'sec.detail.verdict', keys: ['verdict'] },
  { id: 'error', labelKey: 'sec.detail.error', keys: ['error_message', 'error', 'message'] },
  { id: 'reason', labelKey: 'sec.detail.reason', keys: ['reason', 'policy_reason', 'explanation'] },
  { id: 'finding', labelKey: 'sec.detail.finding', keys: ['finding', 'findings'] },
];

export function findDetailValue(value: unknown, keys: string[], depth = 0): unknown {
  if (depth > 5 || value === null || value === undefined) return undefined;
  if (Array.isArray(value)) {
    for (const item of value) {
      const found = findDetailValue(item, keys, depth + 1);
      if (found !== undefined) return found;
    }
    return undefined;
  }
  if (!isRecord(value)) return undefined;

  const wanted = new Set(keys.map((key) => key.toLowerCase()));
  for (const [key, child] of Object.entries(value)) {
    if (wanted.has(key.toLowerCase())) return child;
  }
  for (const child of Object.values(value)) {
    const found = findDetailValue(child, keys, depth + 1);
    if (found !== undefined) return found;
  }
  return undefined;
}

export function securityDetailRows(
  details: unknown,
): Array<{ id: string; labelKey: MessageKey; value: string }> {
  const rows: Array<{ id: string; labelKey: MessageKey; value: string }> = [];
  const seen = new Set<string>();
  for (const field of SECURITY_DETAIL_FIELDS) {
    const value = findDetailValue(details, field.keys);
    if (value === undefined || value === null) continue;
    const preview = recordPreview(value);
    if (preview === '-' || seen.has(`${field.id}:${preview}`)) continue;
    seen.add(`${field.id}:${preview}`);
    rows.push({ id: field.id, labelKey: field.labelKey, value: preview });
  }
  return rows;
}

export function securityEventVerdict(event: SecurityEventRecord): string {
  const value = event.verdict
    ?? findDetailValue(event.details ?? event.details_preview, ['verdict']);
  return value === undefined || value === null ? '-' : recordPreview(value);
}

export type VerdictTone = 'pass' | 'warning' | 'risk' | 'unknown';

export function normalizeVerdict(value: string | null | undefined): string {
  return (value ?? '').trim().toLowerCase();
}

export function verdictTone(value: string | null | undefined): VerdictTone {
  const v = normalizeVerdict(value);
  if (!v || v === '-') return 'unknown';
  if (
    v === 'pass'
    || v === 'allow'
    || v === 'allowed'
    || v === 'ok'
    || v === 'safe'
    || v === 'clean'
    || v.includes('success')
  ) {
    return 'pass';
  }
  if (
    v === 'drifted'
    || v === 'unmanaged'
    || v.includes('warn')
    || v.includes('review')
    || v.includes('caution')
    || v.includes('suspicious')
  ) {
    return 'warning';
  }
  if (
    v === 'tampered'
    || v.includes('deny')
    || v.includes('block')
    || v.includes('fail')
    || v.includes('error')
    || v.includes('risk')
    || v.includes('unsafe')
  ) {
    return 'risk';
  }
  return 'unknown';
}

export function isPassVerdict(value: string | null | undefined): boolean {
  return verdictTone(value) === 'pass';
}

export function verdictBadgeClasses(value: string | null | undefined): string {
  const tone = verdictTone(value);
  if (tone === 'pass') return 'bg-green-100 text-green-700';
  if (tone === 'warning') return 'bg-amber-100 text-amber-800';
  if (tone === 'risk') return 'bg-red-100 text-red-700';
  return 'bg-gray-100 text-gray-700';
}

export function verdictBarClasses(value: string | null | undefined): string {
  const tone = verdictTone(value);
  if (tone === 'pass') return 'bg-green-500';
  if (tone === 'warning') return 'bg-amber-500';
  if (tone === 'risk') return 'bg-red-500';
  return 'bg-gray-400';
}

export function securityPanelClasses(value: string | null | undefined): { panel: string; title: string; button: string; detailLabel: string; detailValue: string } {
  const tone = verdictTone(value);
  if (tone === 'pass') {
    return {
      panel: 'border-green-100 bg-green-50',
      title: 'text-green-950',
      button: 'border-green-200 text-green-700 hover:bg-green-50',
      detailLabel: 'text-green-500',
      detailValue: 'text-green-950',
    };
  }
  if (tone === 'warning') {
    return {
      panel: 'border-amber-100 bg-amber-50',
      title: 'text-amber-950',
      button: 'border-amber-200 text-amber-800 hover:bg-amber-50',
      detailLabel: 'text-amber-500',
      detailValue: 'text-amber-950',
    };
  }
  if (tone === 'risk') {
    return {
      panel: 'border-red-100 bg-red-50',
      title: 'text-red-950',
      button: 'border-red-200 text-red-700 hover:bg-red-50',
      detailLabel: 'text-red-400',
      detailValue: 'text-red-950',
    };
  }
  return {
    panel: 'border-gray-200 bg-gray-50',
    title: 'text-gray-950',
    button: 'border-gray-200 text-gray-700 hover:bg-gray-50',
    detailLabel: 'text-gray-400',
    detailValue: 'text-gray-950',
  };
}

export function verdictCountItems(events: SecurityEventRecord[]): SecurityCountItem[] {
  const counts = new Map<string, number>();
  for (const event of events) {
    const verdict = securityEventVerdict(event);
    if (verdict === '-') continue;
    counts.set(verdict, (counts.get(verdict) ?? 0) + 1);
  }
  return Array.from(counts.entries())
    .map(([value, count]) => ({ value, count }))
    .sort((a, b) => b.count - a.count || String(a.value).localeCompare(String(b.value)));
}

export function fmtPercent(numerator: number, denominator: number): string {
  if (denominator <= 0) return '0%';
  const value = (numerator / denominator) * 100;
  return value >= 10 ? `${Math.round(value)}%` : `${value.toFixed(1)}%`;
}

const STATE_LABEL_KEY: Record<string, MessageKey> = {
  daemon_reachable: 'sec.daemonReachable',
  disabled: 'sec.disabled',
  daemon_unreachable: 'sec.daemonUnreachable',
  store_unavailable: 'sec.storeUnavailable',
  schema_mismatch: 'sec.schemaMismatch',
  ok: 'sec.ok',
  empty: 'sec.empty',
  partial: 'sec.partial',
  found: 'sec.found',
  not_found: 'sec.notFound',
};

export function stateLabelKey(state: string): MessageKey | null {
  return STATE_LABEL_KEY[state] ?? null;
}

export function stateClasses(state: string): string {
  if (state === 'daemon_reachable' || state === 'ok' || state === 'found') {
    return 'bg-green-100 text-green-700 border-green-200';
  }
  if (state === 'empty' || state === 'disabled' || state === 'not_found') {
    return 'bg-gray-100 text-gray-700 border-gray-200';
  }
  if (state === 'partial' || state === 'schema_mismatch' || state === 'store_unavailable') {
    return 'bg-amber-100 text-amber-800 border-amber-200';
  }
  return 'bg-red-100 text-red-700 border-red-200';
}

export function badgeClasses(value: string | null | undefined, kind: 'category' | 'result' | 'kind'): string {
  const v = (value ?? '').toLowerCase();
  if (kind === 'result') {
    if (v.includes('success') || v.includes('allow') || v === 'ok' || v === 'pass') {
      return 'bg-green-100 text-green-700';
    }
    if (v.includes('fail') || v.includes('deny') || v.includes('block') || v.includes('error') || v.includes('risk')) {
      return 'bg-red-100 text-red-700';
    }
    return 'bg-gray-100 text-gray-700';
  }
  if (kind === 'kind') {
    return v === 'security'
      ? 'bg-red-100 text-red-700'
      : 'bg-blue-100 text-blue-700';
  }
  if (v.includes('prompt')) return 'bg-purple-100 text-purple-700';
  if (v.includes('code')) return 'bg-blue-100 text-blue-700';
  if (v.includes('tool')) return 'bg-orange-100 text-orange-700';
  return 'bg-gray-100 text-gray-700';
}
