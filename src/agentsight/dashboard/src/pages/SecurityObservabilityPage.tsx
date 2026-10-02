import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { DateTimePicker } from '../components/DateTimePicker';
import {
  fetchSecurityCountBy,
  fetchSecurityEvent,
  fetchSecurityEvents,
  fetchSecurityRuns,
  fetchSecuritySessions,
  fetchSecurityStatus,
  fetchSecuritySummary,
  fetchSecurityTimeline,
} from '../utils/apiClient';
import type {
  SecurityApiResponse,
  SecurityCountByResponse,
  SecurityEventDetailResponse,
  SecurityEventRecord,
  SecurityPaginated,
  SecurityRunSummary,
  SecuritySessionSummary,
  SecurityStatusData,
  SecuritySummary,
  SecurityTimelineResponse,
  SecurityTimeRangeParams,
} from '../utils/apiClient';
import { EventDetailDrawer } from './security/EventDetailDrawer';
import { EventsTab } from './security/EventsTab';
import { OverviewTab } from './security/OverviewTab';
import { TimelineTab } from './security/TimelineTab';
import { StatePill, StatusPanel } from './security/common';
import {
  EMPTY_EVENT_FILTERS,
  EVENT_PAGE_SIZE,
  OVERVIEW_EVENT_SAMPLE_LIMIT,
  type SecurityTab,
} from './security/types';
import {
  buildObservabilityContextById,
  errorMessage,
  mapToCountItems,
  msToNs,
} from './security/utils';
import { useI18n } from '../i18n';
import type { MessageKey } from '../i18n';

function isSecurityAvailableState(state: string | null | undefined): boolean {
  return state === 'daemon_reachable';
}

export const SecurityObservabilityPage: React.FC = () => {
  const { t } = useI18n();
  const now = Date.now();
  const [startMs, setStartMs] = useState(now - 24 * 3600 * 1000);
  const [endMs, setEndMs] = useState(now);
  const [activeTab, setActiveTab] = useState<SecurityTab>('overview');

  const [status, setStatus] = useState<SecurityApiResponse<SecurityStatusData> | null>(null);
  const [statusLoading, setStatusLoading] = useState(false);
  const [statusError, setStatusError] = useState<string | null>(null);

  const [summary, setSummary] = useState<SecurityApiResponse<SecuritySummary> | null>(null);
  const [categoryCounts, setCategoryCounts] = useState<SecurityApiResponse<SecurityCountByResponse> | null>(null);
  const [eventTypeCounts, setEventTypeCounts] = useState<SecurityApiResponse<SecurityCountByResponse> | null>(null);
  const [resultCounts, setResultCounts] = useState<SecurityApiResponse<SecurityCountByResponse> | null>(null);
  const [verdictCounts, setVerdictCounts] = useState<SecurityApiResponse<SecurityCountByResponse> | null>(null);
  const [recentEvents, setRecentEvents] = useState<SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null>(null);
  const [overviewLoading, setOverviewLoading] = useState(false);
  const [overviewError, setOverviewError] = useState<string | null>(null);

  const [events, setEvents] = useState<SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null>(null);
  const [eventsLoading, setEventsLoading] = useState(false);
  const [eventsError, setEventsError] = useState<string | null>(null);
  const [eventFilters, setEventFilters] = useState(EMPTY_EVENT_FILTERS);
  const [appliedEventFilters, setAppliedEventFilters] = useState(EMPTY_EVENT_FILTERS);

  const [selectedEventId, setSelectedEventId] = useState<string | null>(null);
  const [eventDetail, setEventDetail] = useState<SecurityApiResponse<SecurityEventDetailResponse> | null>(null);
  const [eventDetailLoading, setEventDetailLoading] = useState(false);
  const [eventDetailError, setEventDetailError] = useState<string | null>(null);

  const [securitySessions, setSecuritySessions] = useState<SecurityApiResponse<SecurityPaginated<SecuritySessionSummary>> | null>(null);
  const [sessionsLoading, setSessionsLoading] = useState(false);
  const [sessionsError, setSessionsError] = useState<string | null>(null);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(null);
  const [securityRuns, setSecurityRuns] = useState<SecurityApiResponse<SecurityPaginated<SecurityRunSummary>> | null>(null);
  const [runsLoading, setRunsLoading] = useState(false);
  const [runsError, setRunsError] = useState<string | null>(null);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<SecurityApiResponse<SecurityTimelineResponse> | null>(null);
  const [timelineLoading, setTimelineLoading] = useState(false);
  const [timelineError, setTimelineError] = useState<string | null>(null);
  const [timelineRefreshNonce, setTimelineRefreshNonce] = useState(0);
  const [sessionEvents, setSessionEvents] = useState<SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null>(null);
  const [sessionEventsLoading, setSessionEventsLoading] = useState(false);
  const [sessionEventsError, setSessionEventsError] = useState<string | null>(null);

  const isAvailable = isSecurityAvailableState(status?.state);
  const rangeParams: SecurityTimeRangeParams = useMemo(() => ({
    start_ns: msToNs(startMs),
    end_ns: msToNs(endMs),
  }), [startMs, endMs]);
  const observabilityItemsById = useMemo(
    () => buildObservabilityContextById(timeline?.data.items ?? []),
    [timeline?.data.items],
  );

  const loadStatus = useCallback(async () => {
    setStatusLoading(true);
    setStatusError(null);
    try {
      const nextStatus = await fetchSecurityStatus();
      setStatus(nextStatus);
      return nextStatus;
    } catch (error) {
      setStatus(null);
      setStatusError(errorMessage(error, t));
      return null;
    } finally {
      setStatusLoading(false);
    }
  }, [t]);

  const loadOverview = useCallback(async () => {
    setOverviewLoading(true);
    setOverviewError(null);
    const results = await Promise.allSettled([
      fetchSecuritySummary({ ...rangeParams, latest_limit: 5 }),
      fetchSecurityCountBy('category', rangeParams),
      fetchSecurityCountBy('event_type', rangeParams),
      fetchSecurityCountBy('result', rangeParams),
      fetchSecurityCountBy('verdict', rangeParams),
      fetchSecurityEvents({ ...rangeParams, limit: OVERVIEW_EVENT_SAMPLE_LIMIT, offset: 0, include_details: true }),
      fetchSecuritySessions({ ...rangeParams, limit: 100, offset: 0 }),
    ]);

    const errors: string[] = [];
    const collect = <T,>(
      result: PromiseSettledResult<SecurityApiResponse<T>>,
      setter: (value: SecurityApiResponse<T>) => void,
    ): SecurityApiResponse<T> | null => {
      if (result.status === 'fulfilled') {
        setter(result.value);
        return result.value;
      }
      errors.push(errorMessage(result.reason, t));
      return null;
    };

    collect(results[0] as PromiseSettledResult<SecurityApiResponse<SecuritySummary>>, setSummary);
    collect(results[1] as PromiseSettledResult<SecurityApiResponse<SecurityCountByResponse>>, setCategoryCounts);
    collect(results[2] as PromiseSettledResult<SecurityApiResponse<SecurityCountByResponse>>, setEventTypeCounts);
    collect(results[3] as PromiseSettledResult<SecurityApiResponse<SecurityCountByResponse>>, setResultCounts);
    collect(results[4] as PromiseSettledResult<SecurityApiResponse<SecurityCountByResponse>>, setVerdictCounts);
    collect(results[5] as PromiseSettledResult<SecurityApiResponse<SecurityPaginated<SecurityEventRecord>>>, setRecentEvents);
    const sessionResult = collect(
      results[6] as PromiseSettledResult<SecurityApiResponse<SecurityPaginated<SecuritySessionSummary>>>,
      setSecuritySessions,
    );

    if (sessionResult) {
      const ids = new Set(sessionResult.data.items.map((session) => session.session_id));
      setSelectedSessionId((current) => current && ids.has(current)
        ? current
        : sessionResult.data.items[0]?.session_id ?? null);
    }

    setOverviewError(errors.length > 0 ? errors.join('; ') : null);
    setOverviewLoading(false);
  }, [rangeParams, t]);

  const loadEvents = useCallback(async (offset: number, filters = appliedEventFilters) => {
    if (!isAvailable) return;
    setEventsLoading(true);
    setEventsError(null);
    try {
      const response = await fetchSecurityEvents({
        ...rangeParams,
        ...filters,
        limit: EVENT_PAGE_SIZE,
        offset,
        include_details: true,
      });
      setEvents(response);
    } catch (error) {
      setEventsError(errorMessage(error, t));
    } finally {
      setEventsLoading(false);
    }
  }, [appliedEventFilters, isAvailable, rangeParams, t]);

  const loadSessions = useCallback(async () => {
    if (!isAvailable) return;
    setSessionsLoading(true);
    setSessionsError(null);
    try {
      const response = await fetchSecuritySessions({ ...rangeParams, limit: 100, offset: 0 });
      setSecuritySessions(response);
      const ids = new Set(response.data.items.map((session) => session.session_id));
      setSelectedSessionId((current) => current && ids.has(current)
        ? current
        : response.data.items[0]?.session_id ?? null);
    } catch (error) {
      setSessionsError(errorMessage(error, t));
    } finally {
      setSessionsLoading(false);
    }
  }, [isAvailable, rangeParams, t]);

  const loadEventDetail = useCallback(async (eventId: string) => {
    setEventDetailLoading(true);
    setEventDetailError(null);
    setEventDetail(null);
    try {
      setEventDetail(await fetchSecurityEvent(eventId));
    } catch (error) {
      setEventDetailError(errorMessage(error, t));
    } finally {
      setEventDetailLoading(false);
    }
  }, [t]);

  useEffect(() => {
    loadStatus();
  }, [loadStatus]);

  useEffect(() => {
    if (isAvailable) {
      loadOverview();
    }
  }, [isAvailable, loadOverview]);

  useEffect(() => {
    if (isAvailable && activeTab === 'events') {
      loadEvents(0);
    }
  }, [activeTab, isAvailable, loadEvents]);

  useEffect(() => {
    if (isAvailable && activeTab === 'timeline' && !securitySessions) {
      loadSessions();
    }
  }, [activeTab, isAvailable, loadSessions, securitySessions]);

  useEffect(() => {
    if (!selectedEventId) return;
    loadEventDetail(selectedEventId);
  }, [loadEventDetail, selectedEventId]);

  useEffect(() => {
    if (!isAvailable || activeTab !== 'timeline' || !selectedSessionId) {
      setSessionEvents(null);
      return;
    }

    let cancelled = false;
    setSessionEventsLoading(true);
    setSessionEventsError(null);
    setSessionEvents(null);
    fetchSecurityEvents({
      ...rangeParams,
      session_id: selectedSessionId,
      limit: 500,
      offset: 0,
      include_details: true,
    })
      .then((response) => {
        if (!cancelled) setSessionEvents(response);
      })
      .catch((error) => {
        if (!cancelled) setSessionEventsError(errorMessage(error, t));
      })
      .finally(() => {
        if (!cancelled) setSessionEventsLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [activeTab, isAvailable, rangeParams, selectedSessionId, timelineRefreshNonce, t]);

  useEffect(() => {
    if (!isAvailable || activeTab !== 'timeline' || !selectedSessionId) {
      return;
    }

    let cancelled = false;
    setRunsLoading(true);
    setRunsError(null);
    setSecurityRuns(null);
    setTimeline(null);
    fetchSecurityRuns(selectedSessionId, { ...rangeParams, limit: 100, offset: 0 })
      .then((response) => {
        if (!cancelled) {
          setSecurityRuns(response);
          const ids = new Set(response.data.items.map((run) => run.run_id));
          setSelectedRunId((current) => current && ids.has(current)
            ? current
            : response.data.items[0]?.run_id ?? null);
        }
      })
      .catch((error) => {
        if (!cancelled) {
          setRunsError(errorMessage(error, t));
          setSelectedRunId(null);
        }
      })
      .finally(() => {
        if (!cancelled) setRunsLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [activeTab, isAvailable, rangeParams, selectedSessionId, timelineRefreshNonce, t]);

  useEffect(() => {
    if (!isAvailable || activeTab !== 'timeline' || !selectedSessionId || !selectedRunId) {
      return;
    }

    let cancelled = false;
    setTimelineLoading(true);
    setTimelineError(null);
    fetchSecurityTimeline({
      ...rangeParams,
      session_id: selectedSessionId,
      run_id: selectedRunId,
      limit: 500,
      include_security: true,
    })
      .then((response) => {
        if (!cancelled) setTimeline(response);
      })
      .catch((error) => {
        if (!cancelled) setTimelineError(errorMessage(error, t));
      })
      .finally(() => {
        if (!cancelled) setTimelineLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [activeTab, isAvailable, rangeParams, selectedRunId, selectedSessionId, timelineRefreshNonce, t]);

  const handleRefresh = useCallback(async () => {
    const nextStatus = await loadStatus();
    if (!isSecurityAvailableState(nextStatus?.state)) return;
    await loadOverview();
    if (activeTab === 'events') await loadEvents(0);
    if (activeTab === 'timeline') {
      await loadSessions();
      setTimelineRefreshNonce((current) => current + 1);
    }
  }, [activeTab, loadEvents, loadOverview, loadSessions, loadStatus, t]);

  const overviewEvents = recentEvents?.data.items ?? summary?.data.latest_events ?? [];
  const latestEvents = overviewEvents.slice(0, 10);
  const summaryData = summary?.data;
  const categoryItems = categoryCounts?.data.items ?? mapToCountItems(summaryData?.by_category);
  const eventTypeItems = eventTypeCounts?.data.items ?? mapToCountItems(summaryData?.by_event_type);
  const resultItems = resultCounts?.data.items ?? mapToCountItems(summaryData?.by_result);
  const categoryFilterOptions = Array.from(new Set(categoryItems.map((item) => String(item.value)).filter(Boolean)));
  const resultFilterOptions = Array.from(new Set(resultItems.map((item) => String(item.value)).filter(Boolean)));
  const verdictItems = verdictCounts?.data.items ?? [];
  const verdictFilterOptions = Array.from(new Set(verdictItems.map((item) => String(item.value)).filter(Boolean)));
  const selectedSession = (securitySessions?.data.items ?? []).find((session) => session.session_id === selectedSessionId) ?? null;
  const selectedRun = (securityRuns?.data.items ?? []).find((run) => run.run_id === selectedRunId) ?? null;

  return (
    <main className="mx-auto max-w-screen-xl space-y-6 px-6 py-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="text-2xl font-bold text-gray-900">{t('sec.securityObservability')}</h1>
          <p className="mt-1 text-sm text-gray-500">{t('sec.securityObservabilityDesc')}</p>
        </div>
        <div className="flex items-center gap-2">
          {status && <StatePill state={status.state} />}
          <button
            onClick={handleRefresh}
            disabled={statusLoading || overviewLoading || eventsLoading}
            className="rounded-lg bg-gray-900 px-4 py-2 text-sm font-medium text-white hover:bg-gray-800 disabled:opacity-50"
          >
            {t('sec.refresh')}
          </button>
        </div>
      </div>

      <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
        <div className="flex flex-wrap items-end gap-4">
          <DateTimePicker label={t('common.startTime')} value={startMs} onChange={setStartMs} />
          <DateTimePicker label={t('common.endTime')} value={endMs} onChange={setEndMs} />
          <div className="flex flex-wrap gap-2">
            {[
              { label: t('common.last1h'), ms: 3600 * 1000 },
              { label: t('common.last6h'), ms: 6 * 3600 * 1000 },
              { label: t('common.last24h'), ms: 24 * 3600 * 1000 },
              { label: t('common.last7d'), ms: 7 * 24 * 3600 * 1000 },
            ].map((item) => (
              <button
                key={item.label}
                onClick={() => {
                  const nextEnd = Date.now();
                  setEndMs(nextEnd);
                  setStartMs(nextEnd - item.ms);
                }}
                className="rounded-lg bg-gray-100 px-3 py-1.5 text-xs text-gray-600 hover:bg-gray-200"
              >
                {item.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      <StatusPanel
        status={status}
        loading={statusLoading}
        error={statusError}
        onRetry={handleRefresh}
      />

      {isAvailable && (
        <>
          <div className="flex gap-2 border-b border-gray-200">
            {[
              ['overview', t('sec.overview')],
              ['events', t('sec.securityEvents')],
              ['timeline', t('sec.fullChainEvents')],
            ].map(([key, label]) => (
              <button
                key={key}
                onClick={() => setActiveTab(key as SecurityTab)}
                className={`border-b-2 px-4 py-2 text-sm font-medium ${
                  activeTab === key
                    ? 'border-blue-600 text-blue-700'
                    : 'border-transparent text-gray-500 hover:text-gray-800'
                }`}
              >
                {label}
              </button>
            ))}
          </div>

          {activeTab === 'overview' && (
            <OverviewTab
              overviewError={overviewError}
              overviewLoading={overviewLoading}
              summary={summary}
              summaryData={summaryData}
              recentEvents={recentEvents}
              categoryItems={categoryItems}
              eventTypeItems={eventTypeItems}
              resultItems={resultItems}
              verdictItems={verdictItems}
              latestEvents={latestEvents}
              onSelectEvent={setSelectedEventId}
              onViewVerdict={(verdict) => {
                const next = { ...EMPTY_EVENT_FILTERS, verdict };
                setEventFilters(next);
                setAppliedEventFilters(next);
                setActiveTab('events');
              }}
            />
          )}

          {activeTab === 'events' && (
            <EventsTab
              eventFilters={eventFilters}
              setEventFilters={setEventFilters}
              setAppliedEventFilters={setAppliedEventFilters}
              categoryFilterOptions={categoryFilterOptions}
              resultFilterOptions={resultFilterOptions}
              verdictFilterOptions={verdictFilterOptions}
              events={events}
              eventsLoading={eventsLoading}
              eventsError={eventsError}
              loadEvents={loadEvents}
              onSelectEvent={setSelectedEventId}
              onViewTimeline={(sessionId, runId) => {
                setSelectedSessionId(sessionId);
                setSelectedRunId(runId);
                setActiveTab('timeline');
              }}
            />
          )}

          {activeTab === 'timeline' && (
            <TimelineTab
              selectedSessionId={selectedSessionId}
              setSelectedSessionId={setSelectedSessionId}
              selectedRunId={selectedRunId}
              setSelectedRunId={setSelectedRunId}
              securitySessions={securitySessions}
              sessionsLoading={sessionsLoading}
              sessionsError={sessionsError}
              securityRuns={securityRuns}
              runsLoading={runsLoading}
              runsError={runsError}
              selectedSession={selectedSession}
              selectedRun={selectedRun}
              sessionEvents={sessionEvents}
              sessionEventsLoading={sessionEventsLoading}
              sessionEventsError={sessionEventsError}
              timeline={timeline}
              timelineLoading={timelineLoading}
              timelineError={timelineError}
              observabilityItemsById={observabilityItemsById}
              onSelectEvent={setSelectedEventId}
            />
          )}
        </>
      )}

      {!isAvailable && status && !isSecurityAvailableState(status.state) && (
        <div className="rounded-lg border border-gray-200 bg-white p-6 text-sm text-gray-500">
          {t('sec.currentState', { state: status.state })}
        </div>
      )}

      {selectedEventId && (
        <EventDetailDrawer
          eventId={selectedEventId}
          detail={eventDetail}
          loading={eventDetailLoading}
          error={eventDetailError}
          onClose={() => setSelectedEventId(null)}
          onRetry={() => loadEventDetail(selectedEventId)}
        />
      )}
    </main>
  );
};
