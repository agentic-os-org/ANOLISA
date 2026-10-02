import React from 'react';
import type {
  SecurityApiResponse,
  SecurityCountItem,
  SecurityStatusData,
} from '../../utils/apiClient';
import { useI18n } from '../../i18n';
import { fmtNumber, stateClasses, stateLabelKey } from './utils';

export const StatePill: React.FC<{ state: string }> = ({ state }) => {
  const { t } = useI18n();
  const labelKey = stateLabelKey(state);
  return (
    <span className={`inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium ${stateClasses(state)}`}>
      {labelKey ? t(labelKey) : state}
    </span>
  );
};

export const MetricCard: React.FC<{ label: string; value: number | string; sublabel?: string }> = ({
  label,
  value,
  sublabel,
}) => (
  <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
    <p className="text-sm text-gray-500">{label}</p>
    <p className="mt-1 text-3xl font-bold text-gray-900">{typeof value === 'number' ? fmtNumber(value) : value}</p>
    {sublabel && <p className="mt-1 text-xs text-gray-400">{sublabel}</p>}
  </div>
);

export const StatusPanel: React.FC<{
  status: SecurityApiResponse<SecurityStatusData> | null;
  loading: boolean;
  error: string | null;
  onRetry: () => void;
}> = ({ status, loading, error, onRetry }) => {
  const { t } = useI18n();
  if (loading && !status) {
    return (
      <div className="rounded-lg border border-gray-200 bg-white p-6 text-sm text-gray-500">
        {t('sec.loadingSecurityStatus')}
      </div>
    );
  }

  if (error) {
    return (
      <div className="rounded-lg border border-red-200 bg-red-50 p-5">
        <div className="flex items-start justify-between gap-4">
          <div>
            <p className="text-sm font-semibold text-red-700">{t('sec.statusLoadFailed')}</p>
            <p className="mt-1 text-sm text-red-600">{error}</p>
          </div>
          <button
            onClick={onRetry}
            className="rounded-lg border border-red-300 bg-white px-3 py-1.5 text-sm text-red-700 hover:bg-red-50"
          >
            {t('common.retry')}
          </button>
        </div>
      </div>
    );
  }

  if (!status) return null;

  if (status.state === 'daemon_reachable') {
    return null;
  }

  return (
    <div className={`rounded-lg border p-5 ${
      status.state === 'daemon_reachable'
        ? 'border-green-200 bg-green-50'
        : 'border-amber-200 bg-amber-50'
    }`}>
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="flex items-center gap-2">
            <p className="text-sm font-semibold text-gray-900">{t('sec.daemon')}</p>
            <StatePill state={status.state} />
          </div>
          {status.message && <p className="mt-1 text-sm text-gray-600">{status.message}</p>}
        </div>
        <button
          onClick={onRetry}
          disabled={loading}
          className="rounded-lg border border-gray-300 bg-white px-3 py-1.5 text-sm text-gray-700 hover:bg-gray-50 disabled:opacity-50"
        >
          {loading ? t('sec.refreshing') : t('sec.refreshStatus')}
        </button>
      </div>
    </div>
  );
};

export const DistributionList: React.FC<{
  title: string;
  items: SecurityCountItem[];
  emptyText: string;
}> = ({ title, items, emptyText }) => {
  const max = Math.max(1, ...items.map((item) => item.count));
  return (
    <div className="rounded-lg border border-gray-200 bg-white p-4 shadow-sm">
      <div className="mb-3 flex items-center justify-between gap-3">
        <h3 className="text-sm font-semibold text-gray-900">{title}</h3>
        <span className="text-xs text-gray-400">{fmtNumber(items.reduce((sum, item) => sum + item.count, 0))}</span>
      </div>
      {items.length === 0 ? (
        <div className="flex h-32 items-center justify-center text-sm text-gray-400">{emptyText}</div>
      ) : (
        <div className="space-y-3">
          {items.slice(0, 8).map((item) => (
            <div key={String(item.value)} className="grid grid-cols-[minmax(90px,1fr)_3fr_48px] items-center gap-3">
              <span className="truncate text-xs text-gray-600" title={String(item.value)}>{String(item.value)}</span>
              <div className="h-2 overflow-hidden rounded-full bg-gray-100">
                <div
                  className="h-full rounded-full bg-blue-500"
                  style={{ width: `${Math.max(4, (item.count / max) * 100)}%` }}
                />
              </div>
              <span className="text-right text-xs font-semibold text-gray-700">{fmtNumber(item.count)}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
};
