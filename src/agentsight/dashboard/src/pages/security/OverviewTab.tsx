import React from 'react';
import { useI18n } from '../../i18n';
import type {
  SecurityApiResponse,
  SecurityCountItem,
  SecurityEventRecord,
  SecurityPaginated,
  SecuritySummary,
} from '../../utils/apiClient';
import { DistributionList, MetricCard } from './common';
import { OverviewRiskSummary } from './OverviewRiskSummary';
import { RecentEvents } from './RecentEvents';

export const OverviewTab: React.FC<{
  overviewError: string | null;
  overviewLoading: boolean;
  summary: SecurityApiResponse<SecuritySummary> | null;
  summaryData?: SecuritySummary;
  recentEvents: SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null;
  categoryItems: SecurityCountItem[];
  eventTypeItems: SecurityCountItem[];
  resultItems: SecurityCountItem[];
  verdictItems: SecurityCountItem[];
  latestEvents: SecurityEventRecord[];
  onSelectEvent: (eventId: string) => void;
  onViewVerdict: (verdict: string) => void;
}> = ({
  overviewError,
  overviewLoading,
  summary,
  summaryData,
  recentEvents,
  categoryItems,
  eventTypeItems,
  resultItems,
  verdictItems,
  latestEvents,
  onSelectEvent,
  onViewVerdict,
}) => {
  const { t } = useI18n();
  return (
  <section className="space-y-5">
    {overviewError && (
      <div className="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
        {overviewError}
      </div>
    )}
    {overviewLoading && !summary && (
      <div className="rounded-lg border border-gray-200 bg-white p-6 text-sm text-gray-400">
        {t('sec.loadingSummary')}
      </div>
    )}
    <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
      <MetricCard label={t('sec.securityEventsCount')} value={summaryData?.total ?? 0} />
      <MetricCard label={t('sec.affectedSessions')} value={summaryData?.affected_sessions ?? 0} />
      <MetricCard label={t('sec.affectedRuns')} value={summaryData?.affected_runs ?? 0} />
    </div>
    <OverviewRiskSummary
      summary={summaryData}
      eventsResponse={recentEvents}
      categoryItems={categoryItems}
      resultItems={resultItems}
      verdictItems={verdictItems}
      onViewVerdict={onViewVerdict}
    />
    {summary?.state === 'empty' && (
      <div className="rounded-lg border border-gray-200 bg-gray-50 px-4 py-8 text-center text-sm text-gray-500">
        {t('sec.noEventsInRange')}
      </div>
    )}
    <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
      <DistributionList title={t('sec.byCategory')} items={categoryItems} emptyText={t('sec.noCategoryData')} />
      <DistributionList title={t('sec.byEventType')} items={eventTypeItems} emptyText={t('sec.noEventTypeData')} />
      <DistributionList title={t('sec.byResult')} items={resultItems} emptyText={t('sec.noResultData')} />
    </div>
    <RecentEvents events={latestEvents} onSelect={onSelectEvent} />
  </section>
  );
};
