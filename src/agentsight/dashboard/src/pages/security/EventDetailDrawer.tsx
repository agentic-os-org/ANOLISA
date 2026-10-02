import React from 'react';
import { useI18n, useLocaleTag } from '../../i18n';
import type {
  SecurityApiResponse,
  SecurityEventDetailResponse,
  SecurityEventRecord,
} from '../../utils/apiClient';
import { StatePill } from './common';
import { fmtTime, securityEventVerdict, verdictBadgeClasses } from './utils';

export const EventDetailDrawer: React.FC<{
  eventId: string;
  detail: SecurityApiResponse<SecurityEventDetailResponse> | null;
  loading: boolean;
  error: string | null;
  onClose: () => void;
  onRetry: () => void;
}> = ({ eventId, detail, loading, error, onClose, onRetry }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const event = detail?.data.event;
  return (
    <div className="fixed inset-0 z-50 flex justify-end bg-black bg-opacity-30">
      <div className="flex h-full w-full max-w-3xl flex-col bg-white shadow-2xl">
        <div className="flex items-start justify-between gap-4 border-b border-gray-200 px-5 py-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <h2 className="text-base font-semibold text-gray-900">{t('sec.securityEventDetails')}</h2>
              {detail && <StatePill state={detail.state} />}
            </div>
            <p className="mt-1 truncate font-mono text-xs text-gray-400">{eventId}</p>
          </div>
          <button
            onClick={onClose}
            className="rounded-lg px-2 py-1 text-lg leading-none text-gray-500 hover:bg-gray-100"
            aria-label={t('common.close')}
          >
            x
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-5">
          {loading && <div className="py-10 text-center text-sm text-gray-400">{t('sec.loadingDetails')}</div>}
          {error && (
            <div className="rounded-lg border border-red-200 bg-red-50 p-4">
              <p className="text-sm text-red-700">{error}</p>
              <button
                onClick={onRetry}
                className="mt-3 rounded-lg border border-red-300 bg-white px-3 py-1.5 text-sm text-red-700 hover:bg-red-50"
              >
                {t('common.retry')}
              </button>
            </div>
          )}
          {!loading && !error && detail?.state === 'not_found' && (
            <div className="rounded-lg border border-gray-200 bg-gray-50 p-4 text-sm text-gray-500">
              {t('sec.eventNoLongerExists')}
            </div>
          )}
          {!loading && !error && event && (
            <div className="space-y-5">
              {(event.redacted || detail.state === 'redacted') && (
                <div className="rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-800">
                  {t('sec.redactedByDaemon')}
                </div>
              )}
              {(event.truncated || detail.state === 'truncated') && (
                <div className="rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm text-amber-800">
                  {t('sec.truncatedBySize')}
                </div>
              )}

              <div className="grid gap-3 sm:grid-cols-2">
                {[
                  [t('common.time'), fmtTime(event, locale)],
                  [t('sec.category'), event.category ?? '-'],
                  [t('sec.result'), event.result ?? '-'],
                  [t('sec.verdict'), securityEventVerdict(event)],
                  [t('sec.session'), event.session_id ?? '-'],
                  [t('sec.run'), event.run_id ?? '-'],
                  [t('sec.call'), event.call_id ?? '-'],
                  [t('sec.toolCall'), event.tool_call_id ?? '-'],
                  [t('sec.trace'), event.trace_id ?? '-'],
                ].map(([label, value]) => (
                  <div key={label} className="rounded-lg border border-gray-200 bg-gray-50 px-3 py-2">
                    <p className="text-xs text-gray-400">{label}</p>
                    <p className="mt-1 break-all font-mono text-xs text-gray-800">{value}</p>
                  </div>
                ))}
              </div>

              <div>
                <h3 className="mb-2 text-sm font-semibold text-gray-900">{t('common.details')}</h3>
                <pre className="max-h-[52vh] overflow-auto rounded-lg border border-gray-200 bg-gray-950 p-4 text-xs text-gray-100">
                  {JSON.stringify(event.details ?? event, null, 2)}
                </pre>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
