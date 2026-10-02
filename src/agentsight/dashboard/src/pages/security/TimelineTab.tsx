import React from 'react';
import { useI18n, useLocaleTag } from '../../i18n';
import type {
  SecurityApiResponse,
  SecurityEventRecord,
  SecurityPaginated,
  SecurityRunSummary,
  SecuritySessionSummary,
  SecurityTimelineItem,
  SecurityTimelineResponse,
} from '../../utils/apiClient';
import { TimelineItem } from './TimelineItem';
import { TimelineSessionOverview } from './TimelineSessionOverview';
import { fmtTime, shortId } from './utils';

export const TimelineTab: React.FC<{
  selectedSessionId: string | null;
  setSelectedSessionId: (sessionId: string | null) => void;
  selectedRunId: string | null;
  setSelectedRunId: (runId: string | null) => void;
  securitySessions: SecurityApiResponse<SecurityPaginated<SecuritySessionSummary>> | null;
  sessionsLoading: boolean;
  sessionsError: string | null;
  securityRuns: SecurityApiResponse<SecurityPaginated<SecurityRunSummary>> | null;
  runsLoading: boolean;
  runsError: string | null;
  selectedSession: SecuritySessionSummary | null;
  selectedRun: SecurityRunSummary | null;
  sessionEvents: SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null;
  sessionEventsLoading: boolean;
  sessionEventsError: string | null;
  timeline: SecurityApiResponse<SecurityTimelineResponse> | null;
  timelineLoading: boolean;
  timelineError: string | null;
  observabilityItemsById: Map<string, SecurityTimelineItem>;
  onSelectEvent: (eventId: string) => void;
}> = ({
  selectedSessionId,
  setSelectedSessionId,
  selectedRunId,
  setSelectedRunId,
  securitySessions,
  sessionsLoading,
  sessionsError,
  securityRuns,
  runsLoading,
  runsError,
  selectedSession,
  selectedRun,
  sessionEvents,
  sessionEventsLoading,
  sessionEventsError,
  timeline,
  timelineLoading,
  timelineError,
  observabilityItemsById,
  onSelectEvent,
}) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  return (
  <section className="space-y-4">
    <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
      <div className="grid gap-4 lg:grid-cols-2">
        <label className="text-xs text-gray-500">
          {t('sec.session')}
          <select
            value={selectedSessionId ?? ''}
            onChange={(event) => setSelectedSessionId(event.target.value || null)}
            disabled={sessionsLoading}
            className="mt-1 w-full rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:outline-none focus:ring-2 focus:ring-blue-400"
          >
            <option value="">-</option>
            {(securitySessions?.data.items ?? []).map((session) => (
              <option key={session.session_id} value={session.session_id}>
                {shortId(session.session_id, 40)} · sec {session.security_event_count ?? 0} · obs {session.observability_event_count ?? 0}
              </option>
            ))}
          </select>
        </label>
        <label className="text-xs text-gray-500">
          {t('sec.run')}
          <select
            value={selectedRunId ?? ''}
            onChange={(event) => setSelectedRunId(event.target.value || null)}
            disabled={runsLoading || !selectedSessionId}
            className="mt-1 w-full rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:outline-none focus:ring-2 focus:ring-blue-400"
          >
            <option value="">-</option>
            {(securityRuns?.data.items ?? []).map((run) => (
              <option key={run.run_id} value={run.run_id}>
                {shortId(run.run_id, 40)} · {run.user_input_preview ?? fmtTime(run, locale)}
              </option>
            ))}
          </select>
        </label>
      </div>
      {(sessionsError || runsError) && (
        <div className="mt-3 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-sm text-red-700">
          {sessionsError ?? runsError}
        </div>
      )}
    </div>

    <TimelineSessionOverview
      session={selectedSession}
      run={selectedRun}
      eventsResponse={sessionEvents}
      loading={sessionEventsLoading}
      error={sessionEventsError}
    />

    {timelineError && (
      <div className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700">
        {timelineError}
      </div>
    )}
    {timelineLoading && (
      <div className="rounded-lg border border-gray-200 bg-white p-6 text-center text-sm text-gray-400">
        {t('sec.loadingTimeline')}
      </div>
    )}
    {!timelineLoading && !timelineError && timeline?.state === 'empty' && (
      <div className="rounded-lg border border-gray-200 bg-gray-50 p-6 text-center text-sm text-gray-500">
        {t('sec.noTimelineData')}
      </div>
    )}
    {!timelineLoading && !timelineError && timeline && timeline.data.items.length > 0 && (
      <div className="relative space-y-4 border-l border-gray-200 pl-4">
        {timeline.data.items.map((item, index) => (
          <TimelineItem
            key={`${item.kind}-${item.id ?? item.timestamp_epoch ?? index}`}
            item={item}
            observabilityItemsById={observabilityItemsById}
            onSelectEvent={onSelectEvent}
          />
        ))}
      </div>
    )}
  </section>
  );
};
