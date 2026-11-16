import React, { useCallback, useEffect, useRef, useState } from 'react';
import { fetchCacheDiagnosis } from '../utils/apiClient';
import type { CacheDiagnosis, CacheFinding, CacheGroupDiagnosis, CacheStatus } from '../utils/apiClient';
import { useI18n } from '../i18n';
import type { MessageKey } from '../i18n';

const STATUS_STYLE: Record<CacheStatus, string> = {
  needs_review: 'bg-red-100 text-red-700',
  healthy: 'bg-green-100 text-green-700',
  insufficient_samples: 'bg-gray-100 text-gray-600',
  undeterminable: 'bg-yellow-100 text-yellow-700',
  no_usage: 'bg-gray-100 text-gray-500',
};

const KIND_STYLE: Record<CacheFinding['kind'], string> = {
  suspected: 'text-red-600',
  undeterminable: 'text-yellow-700',
  expected: 'text-gray-500',
};

function percent(rate: number | null): string {
  return rate === null ? '—' : (rate * 100).toFixed(1) + '%';
}

function tokens(count: number | null): string {
  return count === null ? '—' : count.toLocaleString();
}

function milliseconds(value: number | null | undefined): string {
  return value === null || value === undefined ? '—' : Math.round(value).toLocaleString() + ' ms';
}

const FindingRow: React.FC<{ finding: CacheFinding }> = ({ finding }) => {
  const { t } = useI18n();
  const reasonKey = ('cache.reason.' + finding.reason) as MessageKey;
  const adviceKey = ('cache.advice.' + finding.reason) as MessageKey;
  // Unknown reasons from a newer server have no message; fall back to the server's text.
  const reason = t(reasonKey) || finding.reason;
  const advice = t(adviceKey) || finding.recommendation;
  const example = finding.examples[0];
  return (
    <li className="py-1.5">
      <div className="flex flex-wrap items-baseline gap-x-2">
        <span className={`font-medium ${KIND_STYLE[finding.kind]}`}>
          {t(('cache.kind.' + finding.kind) as MessageKey)}
        </span>
        <span className="text-gray-800">{reason}</span>
        <span className="text-gray-500">
          {t('cache.findingStats', {
            calls: finding.calls,
            missed: tokens(finding.missed_tokens),
            unread: tokens(finding.unread_tokens),
            ttft: milliseconds(finding.ttft_ms_p50),
            e2e: milliseconds(finding.e2e_latency_ms_p50),
          })}
        </span>
      </div>
      <div className="text-gray-500">{advice}</div>
      {example?.session_id && (
        <a
          className="text-blue-600 hover:underline"
          href={`#/atif?type=session&id=${encodeURIComponent(example.session_id)}`}
          target="_blank"
          rel="noreferrer"
        >
          {t('cache.example', {
            input: tokens(example.input_tokens),
            read: tokens(example.cache_read_tokens),
            previous: tokens(example.previous_input_tokens),
          })}
        </a>
      )}
    </li>
  );
};

const GroupRow: React.FC<{ group: CacheGroupDiagnosis }> = ({ group }) => {
  const { t } = useI18n();
  const [open, setOpen] = useState(group.status === 'needs_review');
  const baseline = group.baseline;
  const change =
    baseline?.hit_rate_change_pp === null || baseline?.hit_rate_change_pp === undefined
      ? null
      : (baseline.hit_rate_change_pp > 0 ? '+' : '') + baseline.hit_rate_change_pp.toFixed(1) + ' pp';
  return (
    <div className="border-t border-gray-100 py-2 text-[12px]">
      <button
        type="button"
        onClick={() => setOpen(value => !value)}
        className="w-full flex flex-wrap items-center gap-x-3 gap-y-1 text-left"
        aria-expanded={open}
      >
        <span className={`transition-transform text-gray-400 ${open ? 'rotate-90' : ''}`}>▶</span>
        <span className="font-medium text-gray-800">{group.agent_name ?? '—'}</span>
        <span className="text-gray-500">{[group.provider, group.model].filter(Boolean).join(' · ')}</span>
        <span className={`px-1.5 py-0.5 rounded-full text-[11px] ${STATUS_STYLE[group.status]}`}>
          {t(('cache.status.' + group.status) as MessageKey)}
        </span>
        <span title={t('cache.conservativeHint')}>
          <span className="text-gray-400">{t('cache.conservative')}</span> {percent(group.conservative_hit_rate)}
        </span>
        <span title={t('cache.completeHint')}>
          <span className="text-gray-400">{t('cache.complete')}</span> {percent(group.complete_hit_rate)}
        </span>
        {baseline && (
          <span className="text-gray-500">
            {t('cache.baseline', { rate: percent(baseline.conservative_hit_rate), change: change ?? '—' })}
          </span>
        )}
      </button>
      {open && (
        <div className="mt-1.5 ml-5 space-y-1 text-gray-600">
          <div>
            {t('cache.coverage', {
              calls: group.coverage.calls,
              usage: group.coverage.usage_calls,
              read: group.coverage.cache_read_field_calls,
              zero: group.coverage.explicit_zero_read_calls,
            })}
          </div>
          <div>
            {t('cache.tokens', {
              input: tokens(group.tokens.input),
              read: tokens(group.tokens.cache_read),
              write: tokens(group.tokens.cache_creation),
              uncached: tokens(group.tokens.uncached_input),
            })}
          </div>
          <div>
            {t('cache.latency', {
              hit: milliseconds(group.latency.warm.ttft_ms?.p50),
              hitE2e: milliseconds(group.latency.warm.e2e_latency_ms?.p50),
              hitCalls: group.latency.warm.calls,
              miss: milliseconds(group.latency.cold.ttft_ms?.p50),
              missE2e: milliseconds(group.latency.cold.e2e_latency_ms?.p50),
              missCalls: group.latency.cold.calls,
            })}
          </div>
          {group.findings.length > 0 && (
            <ul className="divide-y divide-gray-50">
              {group.findings.map(finding => (
                <FindingRow key={finding.reason} finding={finding} />
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
};

/** Prompt-cache hit rates of the selected window against the preceding window of equal length. */
export const CacheDiagnosisPanel: React.FC<{ rangeMs: number }> = ({ rangeMs }) => {
  const { t } = useI18n();
  const [diagnosis, setDiagnosis] = useState<CacheDiagnosis | null>(null);
  const [error, setError] = useState<string | null>(null);
  const requestIdRef = useRef(0);

  const load = useCallback(async () => {
    const requestId = ++requestIdRef.current;
    try {
      const endNs = Date.now() * 1_000_000;
      const data = await fetchCacheDiagnosis(endNs - rangeMs * 1_000_000, endNs);
      if (requestId === requestIdRef.current) {
        setDiagnosis(data);
        setError(null);
      }
    } catch (e: any) {
      if (requestId === requestIdRef.current) setError(e.message || t('cache.error'));
    }
  }, [rangeMs, t]);

  useEffect(() => {
    void load();
  }, [load]);

  const groups = (diagnosis?.groups ?? []).filter(group => group.status !== 'no_usage');
  return (
    <div className="mt-4 bg-white rounded-lg border border-gray-200 px-4 py-3">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-semibold text-gray-800" title={t('cache.titleHint')}>
          {t('cache.title')}
        </h3>
        {diagnosis?.truncated && <span className="text-[11px] text-yellow-700">{t('cache.truncated')}</span>}
      </div>
      {error !== null ? (
        <div className="py-3 text-[12px] text-red-400">{error}</div>
      ) : diagnosis === null ? (
        <div className="py-3 text-[12px] text-gray-400">{t('common.loading')}</div>
      ) : groups.length === 0 ? (
        <div className="py-3 text-[12px] text-gray-400">{t('cache.empty')}</div>
      ) : (
        groups.map(group => (
          <GroupRow key={[group.agent_name, group.provider, group.model].join('|')} group={group} />
        ))
      )}
    </div>
  );
};
