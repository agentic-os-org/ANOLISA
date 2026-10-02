import React from 'react';
import { useI18n } from '../../i18n';
import type {
  SecurityApiResponse,
  SecurityEventRecord,
  SecurityPaginated,
  SecurityRunSummary,
  SecuritySessionSummary,
} from '../../utils/apiClient';
import {
  fmtNumber,
  isPassVerdict,
  securityEventVerdict,
  shortId,
  verdictBadgeClasses,
  verdictCountItems,
  verdictTone,
} from './utils';

export const TimelineSessionOverview: React.FC<{
  session: SecuritySessionSummary | null;
  run: SecurityRunSummary | null;
  eventsResponse: SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null;
  loading: boolean;
  error: string | null;
}> = ({ session, run, eventsResponse, loading, error }) => {
  const { t } = useI18n();
  const events = eventsResponse?.data.items ?? [];
  const totalSecurityEvents = eventsResponse?.data.total ?? session?.security_event_count ?? 0;
  const verdictItems = verdictCountItems(events);
  const verdictTotal = verdictItems.reduce((sum, item) => sum + item.count, 0);
  const nonPassCount = events.filter((event) => {
    const verdict = securityEventVerdict(event);
    return verdict !== '-' && !isPassVerdict(verdict);
  }).length;
  const hasRiskVerdict = events.some((event) => verdictTone(securityEventVerdict(event)) === 'risk');

  let statusLabel = t('sec.selectSessionToAggregate');
  let statusClasses = 'bg-gray-100 text-gray-700';
  if (loading) {
    statusLabel = t('sec.aggregatingVerdicts');
  } else if (error) {
    statusLabel = t('sec.verdictAggregationFailed');
    statusClasses = 'bg-red-100 text-red-700';
  } else if (!session) {
    statusLabel = t('sec.noSessionSelected');
  } else if (totalSecurityEvents === 0) {
    statusLabel = t('sec.noSecurityEventsShort');
  } else if (verdictTotal === 0) {
    statusLabel = t('sec.noVerdictsYet');
  } else if (nonPassCount === 0) {
    statusLabel = t('sec.allVerdictsPass');
    statusClasses = 'bg-green-100 text-green-700';
  } else {
    statusLabel = t('sec.nonPassVerdicts', { n: nonPassCount });
    statusClasses = hasRiskVerdict ? 'bg-red-100 text-red-700' : 'bg-amber-100 text-amber-800';
  }

  const metrics = [
    [t('sec.session'), shortId(session?.session_id, 18)],
    [t('sec.currentRun'), shortId(run?.run_id, 18)],
    [t('sec.turns'), fmtNumber(session?.turn_count)],
    [t('sec.observabilityEvents'), fmtNumber(session?.observability_event_count)],
    [t('sec.securityEventsCount'), fmtNumber(totalSecurityEvents)],
  ];

  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h3 className="text-sm font-semibold text-gray-900">{t('sec.sessionOverview')}</h3>
          <p className="mt-1 text-xs text-gray-500">{t('sec.sessionOverviewDesc')}</p>
        </div>
        <span className={`rounded px-2 py-0.5 text-xs font-medium ${statusClasses}`}>
          {statusLabel}
        </span>
      </div>

      <div className="mt-4 grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
        {metrics.map(([label, value]) => (
          <div key={label} className="rounded-lg border border-gray-100 bg-gray-50 px-3 py-2">
            <p className="text-xs text-gray-400">{label}</p>
            <p className="mt-1 break-all font-mono text-xs text-gray-800">{value}</p>
          </div>
        ))}
      </div>

      {verdictItems.length > 0 && (
        <div className="mt-3 flex flex-wrap gap-2">
          {verdictItems.map((item) => (
            <span key={String(item.value)} className={`rounded px-2 py-0.5 text-xs font-medium ${verdictBadgeClasses(String(item.value))}`}>
              {String(item.value)} {fmtNumber(item.count)}
            </span>
          ))}
        </div>
      )}
      {error && <p className="mt-3 text-xs text-red-600">{error}</p>}
    </div>
  );
};
