import React, { useCallback, useEffect, useState } from 'react';
import { LlmConfigForm } from '../components/OptimizationSettings';
import { useI18n } from '../i18n';
import {
  fetchStorageStatus,
  StorageSizeState,
  StorageStatusResponse,
  StorageStoreStatus,
} from '../utils/apiClient';

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ['KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 1; index < units.length && value >= 1024; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value.toFixed(value >= 10 ? 1 : 2)} ${unit}`;
}

function stateClass(state: StorageSizeState): string {
  switch (state) {
    case 'cleanup_due':
      return 'bg-red-50 text-red-700 border-red-200';
    case 'reusable_capacity':
      return 'bg-amber-50 text-amber-700 border-amber-200';
    case 'within_policy':
      return 'bg-emerald-50 text-emerald-700 border-emerald-200';
    default:
      return 'bg-gray-50 text-gray-600 border-gray-200';
  }
}

const StorageCard: React.FC = () => {
  const { t } = useI18n();
  const [status, setStatus] = useState<StorageStatusResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      setStatus(await fetchStorageStatus());
      setError(null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const storeLabel = (store: StorageStoreStatus) => {
    switch (store.id) {
      case 'primary': return t('comp.settings.storage.store.primary');
      case 'genai': return t('comp.settings.storage.store.genai');
      case 'interruptions': return t('comp.settings.storage.store.interruptions');
      case 'trajectories': return t('comp.settings.storage.store.trajectories');
      case 'optimization': return t('comp.settings.storage.store.optimization');
      case 'security_audit': return t('comp.settings.storage.store.security_audit');
      case 'enforcement': return t('comp.settings.storage.store.enforcement');
      case 'tokenless': return t('comp.settings.storage.store.tokenless');
      default: return store.id;
    }
  };
  const statusLabel = (store: StorageStoreStatus) => {
    switch (store.size_state) {
      case 'within_policy': return t('comp.settings.storage.state.within_policy');
      case 'cleanup_due': return t('comp.settings.storage.state.cleanup_due');
      case 'reusable_capacity': return t('comp.settings.storage.state.reusable_capacity');
      case 'disabled': return t('comp.settings.storage.state.disabled');
      default: return t('comp.settings.storage.state.unknown');
    }
  };
  const coverageLabel = (store: StorageStoreStatus) => {
    switch (store.coverage) {
      case 'full': return t('comp.settings.storage.coverage.full');
      case 'row_bounded': return t('comp.settings.storage.coverage.row_bounded');
      case 'unmanaged': return t('comp.settings.storage.coverage.unmanaged');
      default: return t('comp.settings.storage.coverage.external');
    }
  };
  const availabilityLabel = (store: StorageStoreStatus) => {
    switch (store.availability) {
      case 'present': return t('comp.settings.storage.availability.present');
      case 'missing': return t('comp.settings.storage.availability.missing');
      case 'error': return t('comp.settings.storage.availability.error');
      default: return t('comp.settings.storage.availability.external');
    }
  };
  const intervalLabel = (store: StorageStoreStatus) => {
    if (store.policy.check_interval === 0) return t('comp.settings.storage.disabled');
    if (store.policy.check_interval_unit === 'inserts') {
      return t('comp.settings.storage.inserts', { count: store.policy.check_interval });
    }
    if (store.policy.check_interval_unit === 'seconds') {
      return t('comp.settings.storage.seconds', { count: store.policy.check_interval });
    }
    return t('comp.settings.storage.external');
  };

  return (
    <section className="bg-white rounded-xl border border-gray-200 shadow-sm">
      <div className="px-6 py-4 border-b border-gray-200">
        <h2 className="text-lg font-semibold text-gray-900">{t('comp.settings.storage.title')}</h2>
        <p className="text-xs text-gray-500 mt-0.5">{t('comp.settings.storage.description')}</p>
      </div>

      {loading ? (
        <div className="px-6 py-10 text-sm text-gray-500">{t('comp.settings.storage.loading')}</div>
      ) : error ? (
        <div className="px-6 py-6">
          <p className="text-sm text-red-600">{t('comp.settings.storage.loadFailed', { error })}</p>
          <button
            type="button"
            className="mt-3 px-3 py-1.5 text-sm rounded-lg border border-gray-300 hover:bg-gray-50"
            onClick={() => void load()}
          >
            {t('comp.settings.storage.retry')}
          </button>
        </div>
      ) : (
        <div className="divide-y divide-gray-100">
          {status?.stores.map((store) => (
            <div key={store.id} className="px-6 py-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div>
                  <h3 className="text-sm font-semibold text-gray-900">{storeLabel(store)}</h3>
                  <p className="text-xs text-gray-500 mt-0.5">
                    {coverageLabel(store)}
                  </p>
                </div>
                <span className={`text-xs px-2 py-1 rounded-full border ${stateClass(store.size_state)}`}>
                  {statusLabel(store)}
                </span>
              </div>

              <dl className="mt-3 grid grid-cols-2 md:grid-cols-3 gap-3 text-xs">
                <div>
                  <dt className="text-gray-500">{t('comp.settings.storage.physical')}</dt>
                  <dd className="mt-1 font-medium text-gray-800">
                    {store.size ? formatBytes(store.size.physical_bytes) : availabilityLabel(store)}
                  </dd>
                </div>
                <div>
                  <dt className="text-gray-500">{t('comp.settings.storage.logical')}</dt>
                  <dd className="mt-1 font-medium text-gray-800">
                    {store.size ? formatBytes(store.size.logical_bytes) : '—'}
                  </dd>
                </div>
                <div>
                  <dt className="text-gray-500">{t('comp.settings.storage.retention')}</dt>
                  <dd className="mt-1 font-medium text-gray-800">
                    {store.policy.retention_days > 0
                      ? t('comp.settings.storage.days', { count: store.policy.retention_days })
                      : t('comp.settings.storage.disabled')}
                  </dd>
                </div>
                <div>
                  <dt className="text-gray-500">{t('comp.settings.storage.limit')}</dt>
                  <dd className="mt-1 font-medium text-gray-800">
                    {store.policy.size_limit_bytes > 0
                      ? formatBytes(store.policy.size_limit_bytes)
                      : t('comp.settings.storage.disabled')}
                  </dd>
                </div>
                <div>
                  <dt className="text-gray-500">{t('comp.settings.storage.interval')}</dt>
                  <dd className="mt-1 font-medium text-gray-800">{intervalLabel(store)}</dd>
                </div>
                <div>
                  <dt className="text-gray-500">{t('comp.settings.storage.owner')}</dt>
                  <dd className="mt-1 font-medium text-gray-800">{store.policy.enforced_by}</dd>
                </div>
              </dl>
            </div>
          ))}
          <p className="px-6 py-4 text-xs text-gray-500 bg-gray-50 rounded-b-xl">
            {t('comp.settings.storage.note')}
          </p>
        </div>
      )}
    </section>
  );
};

/** Standalone settings page hosting global dashboard configuration sections. */
export const SettingsPage: React.FC = () => {
  const { t } = useI18n();
  return (
    <div className="max-w-3xl mx-auto px-6 py-8">
      <div className="mb-6">
        <h1 className="text-2xl font-bold text-gray-900">{t('comp.settings.title')}</h1>
        <p className="text-sm text-gray-500 mt-1">{t('comp.settings.description')}</p>
      </div>

      <div className="space-y-6">
        <StorageCard />
        <LlmConfigForm />
      </div>
    </div>
  );
};
