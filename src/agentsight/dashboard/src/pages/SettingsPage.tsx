import React, { useCallback, useEffect, useRef, useState } from 'react';
import { LlmConfigForm, LlmConfigFormHandle } from '../components/OptimizationSettings';
import { useI18n } from '../i18n';
import { fetchStorageStatus, saveStorageLimit } from '../utils/apiClient';

const MIN_STORAGE_LIMIT_MB = 9;

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

/** Save trigger exposed to the settings page's unified save button. */
export interface StorageCardHandle {
  /** Persists the current limit input; resolves to false on failure. */
  save: () => Promise<boolean>;
}

/** Storage settings card: one combined size limit for all SQLite databases. */
const StorageCard = React.forwardRef<StorageCardHandle>((_, ref) => {
  const { t } = useI18n();
  const [limitMb, setLimitMb] = useState<number | null>(null);
  const [input, setInput] = useState('');
  const [totalBytes, setTotalBytes] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const status = await fetchStorageStatus();
      setLimitMb(status.max_total_size_mb);
      setInput(String(status.max_total_size_mb));
      setTotalBytes(status.total_physical_bytes);
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

  const save = useCallback(async (): Promise<boolean> => {
    const value = Number(input);
    if (!Number.isInteger(value) || value < 0 || (value > 0 && value < MIN_STORAGE_LIMIT_MB)) {
      setError(t('comp.settings.storage.invalid'));
      return false;
    }
    try {
      await saveStorageLimit(value);
      setLimitMb(value);
      setError(null);
      const status = await fetchStorageStatus();
      setTotalBytes(status.total_physical_bytes);
      return true;
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      return false;
    }
  }, [input, t]);

  React.useImperativeHandle(ref, () => ({ save }));

  return (
    <section className="bg-white rounded-xl border border-gray-200 shadow-sm">
      <div className="px-6 py-4 border-b border-gray-200">
        <h2 className="text-lg font-semibold text-gray-900">{t('comp.settings.storage.title')}</h2>
        <p className="text-xs text-gray-500 mt-0.5">{t('comp.settings.storage.description')}</p>
      </div>

      {loading ? (
        <div className="px-6 py-10 text-sm text-gray-500">{t('comp.settings.storage.loading')}</div>
      ) : error && limitMb === null ? (
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
        <div className="px-6 py-5 space-y-4">
          <p className="text-sm text-gray-700">
            {t('comp.settings.storage.currentUsage', {
              used: totalBytes === null ? '—' : formatBytes(totalBytes),
              limit:
                limitMb === null || limitMb === 0
                  ? t('comp.settings.storage.unlimited')
                  : formatBytes(limitMb * 1024 * 1024),
            })}
          </p>

          <div className="flex items-center gap-3">
            <label htmlFor="storage-limit-mb" className="text-sm text-gray-700 whitespace-nowrap">
              {t('comp.settings.storage.limitLabel')}
            </label>
            <input
              id="storage-limit-mb"
              type="number"
              min="0"
              step="1"
              value={input}
              onChange={(event) => {
                setInput(event.target.value);
                setError(null);
              }}
              className="w-32 border border-gray-300 rounded-lg px-3 py-1.5 text-sm focus:outline-none focus:ring-2 focus:ring-blue-400"
            />
            <span className="text-sm text-gray-500">MB</span>
          </div>

          {error && limitMb !== null && <p className="text-sm text-red-600">{error}</p>}
          <p className="text-xs text-gray-500">{t('comp.settings.storage.hint')}</p>
        </div>
      )}
    </section>
  );
});

StorageCard.displayName = 'StorageCard';

/** Standalone settings page hosting global dashboard configuration sections. */
export const SettingsPage: React.FC = () => {
  const { t } = useI18n();
  const storageRef = useRef<StorageCardHandle>(null);
  const llmRef = useRef<LlmConfigFormHandle>(null);
  const [saving, setSaving] = useState(false);
  const [result, setResult] = useState<'idle' | 'saved' | 'failed'>('idle');

  const saveAll = async () => {
    setSaving(true);
    setResult('idle');
    const [storageOk, llmOk] = await Promise.all([
      storageRef.current?.save() ?? Promise.resolve(true),
      llmRef.current?.save() ?? Promise.resolve(true),
    ]);
    setResult(storageOk && llmOk ? 'saved' : 'failed');
    setSaving(false);
  };

  return (
    <div className="max-w-3xl mx-auto px-6 py-8">
      <div className="mb-6">
        <h1 className="text-2xl font-bold text-gray-900">{t('comp.settings.title')}</h1>
        <p className="text-sm text-gray-500 mt-1">{t('comp.settings.description')}</p>
      </div>

      <div className="space-y-6">
        <StorageCard ref={storageRef} />
        <LlmConfigForm ref={llmRef} />

        <div className="flex items-center gap-3">
          <button
            type="button"
            disabled={saving}
            onClick={() => void saveAll()}
            className="px-5 py-2 bg-blue-600 text-white rounded-lg text-sm font-medium hover:bg-blue-700 transition-colors disabled:opacity-50"
          >
            {saving ? t('comp.settings.saving') : t('comp.settings.save')}
          </button>
          {result === 'saved' && (
            <span className="text-sm text-emerald-600">{t('comp.settings.saved')}</span>
          )}
          {result === 'failed' && (
            <span className="text-sm text-red-600">{t('comp.settings.saveFailed')}</span>
          )}
        </div>
      </div>
    </div>
  );
};
