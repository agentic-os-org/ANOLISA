import React from 'react';
import { useI18n } from '../../i18n';
import type {
  SecurityApiResponse,
  SecurityEventRecord,
  SecurityPaginated,
} from '../../utils/apiClient';
import { EventTable } from './EventTable';
import { EMPTY_EVENT_FILTERS, type SecurityEventFilters } from './types';

export const EventsTab: React.FC<{
  eventFilters: SecurityEventFilters;
  setEventFilters: React.Dispatch<React.SetStateAction<SecurityEventFilters>>;
  setAppliedEventFilters: React.Dispatch<React.SetStateAction<SecurityEventFilters>>;
  categoryFilterOptions: string[];
  resultFilterOptions: string[];
  verdictFilterOptions: string[];
  events: SecurityApiResponse<SecurityPaginated<SecurityEventRecord>> | null;
  eventsLoading: boolean;
  eventsError: string | null;
  loadEvents: (offset: number) => void;
  onSelectEvent: (eventId: string) => void;
  onViewTimeline: (sessionId: string, runId: string) => void;
}> = ({
  eventFilters,
  setEventFilters,
  setAppliedEventFilters,
  categoryFilterOptions,
  resultFilterOptions,
  verdictFilterOptions,
  events,
  eventsLoading,
  eventsError,
  loadEvents,
  onSelectEvent,
  onViewTimeline,
}) => {
  const { t } = useI18n();
  return (
  <section className="space-y-4">
    <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-6">
        <label className="text-xs text-gray-500">
          {t('sec.category')}
          <select
            value={eventFilters.category}
            onChange={(event) => setEventFilters((current) => ({
              ...current,
              category: event.target.value,
            }))}
            className="mt-1 w-full rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:outline-none focus:ring-2 focus:ring-blue-400"
          >
            <option value="">{t('sec.all')}</option>
            {categoryFilterOptions.map((value) => (
              <option key={value} value={value}>{value}</option>
            ))}
          </select>
        </label>
        <label className="text-xs text-gray-500">
          {t('sec.result')}
          <select
            value={eventFilters.result}
            onChange={(event) => setEventFilters((current) => ({
              ...current,
              result: event.target.value,
            }))}
            className="mt-1 w-full rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:outline-none focus:ring-2 focus:ring-blue-400"
          >
            <option value="">{t('sec.all')}</option>
            {resultFilterOptions.map((value) => (
              <option key={value} value={value}>{value}</option>
            ))}
          </select>
        </label>
        <label className="text-xs text-gray-500">
          {t('sec.verdict')}
          <select
            value={eventFilters.verdict}
            onChange={(event) => setEventFilters((current) => ({
              ...current,
              verdict: event.target.value,
            }))}
            className="mt-1 w-full rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:outline-none focus:ring-2 focus:ring-blue-400"
          >
            <option value="">{t('sec.all')}</option>
            {verdictFilterOptions.map((value) => (
              <option key={value} value={value}>{value}</option>
            ))}
          </select>
        </label>
        {[
          ['session_id', t('sec.sessionId')],
          ['run_id', t('sec.runId')],
        ].map(([key, label]) => (
          <label key={key} className="text-xs text-gray-500">
            {label}
            <input
              value={eventFilters[key as keyof SecurityEventFilters]}
              onChange={(event) => setEventFilters((current) => ({
                ...current,
                [key]: event.target.value,
              }))}
              className="mt-1 w-full rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-800 focus:outline-none focus:ring-2 focus:ring-blue-400"
            />
          </label>
        ))}
        <div className="flex items-end gap-2">
          <button
            onClick={() => {
              setAppliedEventFilters(eventFilters);
            }}
            disabled={eventsLoading}
            className="w-full rounded-lg bg-blue-600 px-3 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
          >
            {t('common.query')}
          </button>
          <button
            onClick={() => {
              setEventFilters(EMPTY_EVENT_FILTERS);
              setAppliedEventFilters(EMPTY_EVENT_FILTERS);
            }}
            className="rounded-lg border border-gray-300 px-3 py-2 text-sm text-gray-700 hover:bg-gray-50"
          >
            {t('sec.clear')}
          </button>
        </div>
      </div>
    </div>
    <EventTable
      response={events}
      loading={eventsLoading}
      error={eventsError}
      onSelect={onSelectEvent}
      onPage={loadEvents}
      onViewTimeline={onViewTimeline}
    />
  </section>
  );
};
