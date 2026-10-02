import React from 'react';
import { useI18n, useLocaleTag } from '../../i18n';
import type { SecurityTimelineItem } from '../../utils/apiClient';
import {
  badgeClasses,
  fmtTime,
  recordPreview,
  securityDetailRows,
  securityEventVerdict,
  securityPanelClasses,
  shortId,
  timelineObservabilityContext,
  verdictBadgeClasses,
} from './utils';

export const TimelineItem: React.FC<{
  item: SecurityTimelineItem;
  observabilityItemsById: Map<string, SecurityTimelineItem>;
  onSelectEvent: (eventId: string) => void;
}> = ({ item, observabilityItemsById, onSelectEvent }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const securityEvent = item.kind === 'security' ? item.event : undefined;
  const observabilityContext = timelineObservabilityContext(item, observabilityItemsById);
  const eventTitle = securityEvent?.event_type ?? securityEvent?.event_id;
  const title = item.title
    ?? (securityEvent && observabilityContext.hook ? `${eventTitle ?? 'security'} @ ${observabilityContext.hook}` : null)
    ?? item.hook
    ?? eventTitle
    ?? item.kind;
  const correlated = Array.isArray(item.correlated_security_events)
    ? item.correlated_security_events
    : [];
  const securityDetails = securityEvent?.details ?? securityEvent?.details_preview;
  const detailRows = securityEvent ? securityDetailRows(securityDetails) : [];
  const securityVerdict = securityEvent ? securityEventVerdict(securityEvent) : '-';
  const securityClasses = securityPanelClasses(securityVerdict);
  const sessionId = item.session_id ?? observabilityContext.session_id ?? securityEvent?.session_id;
  const runId = item.run_id ?? observabilityContext.run_id ?? securityEvent?.run_id;
  const toolCallId = item.tool_call_id ?? observabilityContext.tool_call_id ?? securityEvent?.tool_call_id;
  const itemMetadata = item.metadata ?? observabilityContext.metadata;
  const itemMetrics = item.metrics ?? observabilityContext.metrics;

  return (
    <div className="relative pl-6">
      <div className="absolute left-0 top-2 h-3 w-3 rounded-full border-2 border-white bg-blue-500 shadow ring-2 ring-blue-100" />
      <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <span className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClasses(item.kind, 'kind')}`}>
                {item.kind}
              </span>
              <h4 className="truncate text-sm font-semibold text-gray-900">{title}</h4>
              {securityEvent && observabilityContext.hook && (
                <span className="rounded bg-blue-50 px-2 py-0.5 text-xs font-medium text-blue-700">
                  {t('sec.observabilityHook', { hook: observabilityContext.hook })}
                </span>
              )}
              {item.redacted && <span className="rounded bg-amber-100 px-2 py-0.5 text-xs text-amber-800">{t('sec.redacted')}</span>}
              {item.truncated && <span className="rounded bg-amber-100 px-2 py-0.5 text-xs text-amber-800">{t('sec.truncated')}</span>}
            </div>
            <p className="mt-1 text-xs text-gray-500">{fmtTime(item, locale)}</p>
          </div>
          {item.match && (
            <span className="rounded bg-green-100 px-2 py-0.5 text-xs text-green-700">
              {t('sec.matchReason', { reason: recordPreview(item.match.reason) })}
            </span>
          )}
        </div>

        {item.summary && <p className="mt-3 text-sm text-gray-700">{item.summary}</p>}
        {securityEvent && (
          <div className={`mt-3 rounded-lg border p-3 ${securityClasses.panel}`}>
            <div className="flex flex-wrap items-center gap-2">
              <span className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClasses(securityEvent.category, 'category')}`}>
                {securityEvent.category ?? '-'}
              </span>
              <span className={`text-sm font-medium ${securityClasses.title}`}>{securityEvent.event_type ?? securityEvent.event_id}</span>
              <span className={`rounded px-2 py-0.5 text-xs font-medium ${badgeClasses(securityEvent.result, 'result')}`}>
                {securityEvent.result ?? '-'}
              </span>
              <span className={`rounded px-2 py-0.5 text-xs font-medium ${verdictBadgeClasses(securityVerdict)}`}>
                {securityVerdict}
              </span>
              <button
                onClick={() => onSelectEvent(securityEvent.event_id)}
                className={`ml-auto rounded border bg-white px-2 py-1 text-xs font-medium ${securityClasses.button}`}
                aria-label={t('sec.viewEventDetailsAria', { id: securityEvent.event_id })}
              >
                {t('common.details')}
              </button>
            </div>
            {detailRows.length > 0 && (
              <div className="mt-3 grid gap-2 sm:grid-cols-2">
                {detailRows.map((row) => (
                  <div key={row.id} className="rounded border border-white/60 bg-white/70 px-3 py-2">
                    <p className={`text-xs ${securityClasses.detailLabel}`}>{t(row.labelKey)}</p>
                    <p className={`mt-1 break-words font-mono text-xs ${row.id === 'verdict' ? verdictBadgeClasses(row.value) : securityClasses.detailValue} ${row.id === 'verdict' ? 'inline-block rounded px-2 py-0.5' : ''}`}>
                      {row.value}
                    </p>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
        {correlated.length > 0 && (
          <div className="mt-3 rounded-lg border border-red-100 bg-red-50 p-3 text-sm text-red-800">
            {t('sec.correlatedEvents', { n: correlated.length })}
          </div>
        )}

        <div className="mt-3 grid gap-2 text-xs text-gray-500 sm:grid-cols-3">
          <span className="font-mono">{t('sec.sessionLabel', { id: shortId(sessionId) })}</span>
          <span className="font-mono">{t('sec.runLabel', { id: shortId(runId) })}</span>
          <span className="font-mono">{t('sec.toolLabel', { id: shortId(toolCallId) })}</span>
        </div>

        {(itemMetadata || itemMetrics) && (
          <details className="mt-3">
            <summary className="cursor-pointer text-xs text-gray-500">{t('sec.metadataMetrics')}</summary>
            <pre className="mt-2 max-h-60 overflow-auto rounded bg-gray-950 p-3 text-xs text-gray-100">
              {JSON.stringify({ metadata: itemMetadata, metrics: itemMetrics }, null, 2)}
            </pre>
          </details>
        )}
      </div>
    </div>
  );
};
