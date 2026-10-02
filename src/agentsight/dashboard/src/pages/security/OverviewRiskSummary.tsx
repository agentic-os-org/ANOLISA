import React from 'react';
import { useI18n } from '../../i18n';
import type {
  SecurityApiResponse,
  SecurityCountItem,
  SecurityEventRecord,
  SecurityPaginated,
  SecuritySummary,
} from '../../utils/apiClient';
import {
  badgeClasses,
  fmtNumber,
  fmtPercent,
  isPassVerdict,
  verdictBadgeClasses,
  verdictBarClasses,
  verdictTone,
} from './utils';

export const OverviewRiskSummary: React.FC<{
  summary: SecuritySummary | undefined;
  eventsResponse: SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null;
  categoryItems: SecurityCountItem[];
  resultItems: SecurityCountItem[];
  verdictItems: SecurityCountItem[];
  onViewVerdict?: (verdict: string) => void;
}> = ({ summary, eventsResponse, categoryItems, resultItems, verdictItems, onViewVerdict }) => {
  const { t } = useI18n();
  const totalEvents = eventsResponse?.data.total ?? summary?.total ?? 0;
  const verdictTotal = verdictItems.reduce((sum, item) => sum + item.count, 0);
  const maxVerdictCount = Math.max(1, ...verdictItems.map((item) => item.count));
  const nonPassCount = verdictItems
    .filter((item) => {
      const verdict = String(item.value);
      return verdict !== '-' && !isPassVerdict(verdict);
    })
    .reduce((sum, item) => sum + item.count, 0);
  const riskCount = verdictItems
    .filter((item) => verdictTone(String(item.value)) === 'risk')
    .reduce((sum, item) => sum + item.count, 0);
  const warningCount = verdictItems
    .filter((item) => verdictTone(String(item.value)) === 'warning')
    .reduce((sum, item) => sum + item.count, 0);
  const nonPassRatio = fmtPercent(nonPassCount, verdictTotal);
  const coverageText = t('sec.verdictCoverage', { n: fmtNumber(verdictTotal) });

  let statusLabel = t('sec.noVerdictsYet');
  let statusClasses = 'bg-gray-100 text-gray-700';
  let summaryText = totalEvents === 0
    ? t('sec.summaryNoEvents')
    : t('sec.summaryChecksPerformed', { n: fmtNumber(totalEvents) });
  if (verdictTotal > 0 && nonPassCount === 0) {
    statusLabel = t('sec.noNonPassFound');
    statusClasses = 'bg-green-100 text-green-700';
    summaryText = t('sec.summaryAllPass', { n: fmtNumber(totalEvents) });
  } else if (riskCount > 0) {
    statusLabel = t('sec.riskVerdictsFound', { n: fmtNumber(riskCount) });
    statusClasses = 'bg-red-100 text-red-700';
    summaryText = t('sec.summaryRiskFound', { total: fmtNumber(totalEvents), nonPass: fmtNumber(nonPassCount) });
  } else if (warningCount > 0 || nonPassCount > 0) {
    statusLabel = t('sec.verdictsToReview', { n: fmtNumber(nonPassCount) });
    statusClasses = 'bg-amber-100 text-amber-800';
    summaryText = t('sec.summaryReviewRecommended', { total: fmtNumber(totalEvents), nonPass: fmtNumber(nonPassCount) });
  } else if (totalEvents > 0) {
    summaryText = t('sec.summaryNoVerdictDetails', { n: fmtNumber(totalEvents) });
  }

  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold text-gray-900">{t('sec.capabilityVerdict')}</h3>
          <p className="mt-1 text-sm text-gray-600">{summaryText}</p>
          <p className="mt-1 text-xs text-gray-400">{coverageText}</p>
        </div>
        <span className={`rounded px-2 py-0.5 text-xs font-medium ${statusClasses}`}>{statusLabel}</span>
      </div>

      <div className="mt-4 grid gap-3 sm:grid-cols-3">
        <div className="rounded-lg border border-gray-100 bg-gray-50 px-3 py-2">
          <p className="text-xs text-gray-400">{t('sec.checksExecuted')}</p>
          <p className="mt-1 text-xl font-semibold text-gray-900">{fmtNumber(totalEvents)}</p>
          <p className="mt-1 text-xs text-gray-500">
            {t('sec.coveringSessionsRuns', { sessions: fmtNumber(summary?.affected_sessions), runs: fmtNumber(summary?.affected_runs) })}
          </p>
        </div>
        <div className="rounded-lg border border-gray-100 bg-gray-50 px-3 py-2">
          <p className="text-xs text-gray-400">{t('sec.riskyOperationRatio')}</p>
          <p className={`mt-1 text-xl font-semibold ${nonPassCount > 0 ? 'text-red-700' : 'text-green-700'}`}>{nonPassRatio}</p>
          <p className="mt-1 text-xs text-gray-500">
            {t('sec.verdictsNotPass', { nonPass: fmtNumber(nonPassCount), total: fmtNumber(verdictTotal) })}
          </p>
        </div>
        <div className="rounded-lg border border-gray-100 bg-gray-50 px-3 py-2">
          <p className="text-xs text-gray-400">{t('sec.needsAttention')}</p>
          <p className="mt-1 text-xl font-semibold text-gray-900">{fmtNumber(riskCount + warningCount)}</p>
          <p className="mt-1 text-xs text-gray-500">
            {t('sec.riskWarningCount', { risk: fmtNumber(riskCount), warning: fmtNumber(warningCount) })}
          </p>
        </div>
      </div>

      <div className="mt-4 grid gap-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)]">
        <div>
          <div className="flex items-center justify-between gap-3">
            <h4 className="text-xs font-semibold uppercase text-gray-500">{t('sec.clusteredByVerdict')}</h4>
            {verdictTotal > 0 && <span className="text-xs text-gray-400">{t('sec.withVerdict', { n: fmtNumber(verdictTotal) })}</span>}
          </div>
          {verdictItems.length === 0 ? (
            <div className="mt-3 rounded-lg border border-gray-100 bg-gray-50 px-3 py-6 text-center text-sm text-gray-400">
              {t('sec.noVerdictClusterData')}
            </div>
          ) : (
            <div className="mt-3 space-y-3">
              {verdictItems.map((item) => {
                const verdictValue = String(item.value);
                const isNonPass = !isPassVerdict(verdictValue) && verdictValue !== '-';
                return (
                <div key={verdictValue} className="grid grid-cols-[auto_auto_1fr_88px] items-center gap-3">
                  <span className={`w-fit rounded px-2 py-0.5 text-xs font-medium ${verdictBadgeClasses(verdictValue)}`}>
                    {verdictValue}
                  </span>
                  {isNonPass && onViewVerdict ? (
                    <button
                      onClick={() => onViewVerdict(verdictValue)}
                      className="whitespace-nowrap text-xs text-blue-600 hover:text-blue-800 hover:underline"
                    >
                      {t('common.details')}
                    </button>
                  ) : (
                    <span />
                  )}
                  <div className="h-2 overflow-hidden rounded-full bg-gray-100">
                    <div
                      className={`h-full rounded-full ${verdictBarClasses(verdictValue)}`}
                      style={{ width: `${Math.max(4, (item.count / maxVerdictCount) * 100)}%` }}
                    />
                  </div>
                  <span className="text-right text-xs font-semibold text-gray-700">
                    {fmtNumber(item.count)} · {fmtPercent(item.count, verdictTotal)}
                  </span>
                </div>
                );
              })}
            </div>
          )}
        </div>

        <div className="space-y-3">
          <div>
            <h4 className="text-xs font-semibold uppercase text-gray-500">{t('sec.whatCapabilityDid')}</h4>
            <div className="mt-2 flex flex-wrap gap-2">
              {categoryItems.slice(0, 6).map((item) => (
                <span key={String(item.value)} className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClasses(String(item.value), 'category')}`}>
                  {String(item.value)} {fmtNumber(item.count)}
                </span>
              ))}
              {categoryItems.length === 0 && <span className="text-xs text-gray-400">{t('sec.noCheckActionData')}</span>}
            </div>
          </div>
          <div>
            <h4 className="text-xs font-semibold uppercase text-gray-500">{t('sec.executionStatus')}</h4>
            <div className="mt-2 flex flex-wrap gap-2">
              {resultItems.slice(0, 6).map((item) => (
                <span key={String(item.value)} className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClasses(String(item.value), 'result')}`}>
                  {String(item.value)} {fmtNumber(item.count)}
                </span>
              ))}
              {resultItems.length === 0 && <span className="text-xs text-gray-400">{t('sec.noExecutionStatusData')}</span>}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};
