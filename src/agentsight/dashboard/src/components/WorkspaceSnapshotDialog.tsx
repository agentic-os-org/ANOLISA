import React, { useEffect, useRef, useState } from 'react';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';
import {
  ApiRequestError,
  fetchWorkspaceSnapshotDetail,
  type SnapshotDiffEntry,
  type WorkspaceSnapshotDetailRequest,
  type WorkspaceSnapshotDetailResponse,
} from '../utils/apiClient';

interface WorkspaceSnapshotDialogProps {
  request: WorkspaceSnapshotDetailRequest | null;
  onClose: () => void;
}

const CHANGE_LABELS: Record<SnapshotDiffEntry['change_type'], MessageKey> = {
  Added: 'atif.snapshotChangeAdded',
  Modified: 'atif.snapshotChangeModified',
  Deleted: 'atif.snapshotChangeDeleted',
  Renamed: 'atif.snapshotChangeRenamed',
};

const CHANGE_STYLES: Record<SnapshotDiffEntry['change_type'], string> = {
  Added: 'bg-green-100 text-green-700',
  Modified: 'bg-amber-100 text-amber-700',
  Deleted: 'bg-red-100 text-red-700',
  Renamed: 'bg-blue-100 text-blue-700',
};

function errorMessage(error: unknown, t: (key: MessageKey) => string): string {
  if (error instanceof ApiRequestError && typeof error.body?.message === 'string') {
    return error.body.message;
  }
  return t('atif.snapshotDetailLoadFailed');
}

function formatTime(value: string, locale: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString(locale);
}

const DiffList: React.FC<{ title: string; entries: SnapshotDiffEntry[] }> = ({ title, entries }) => {
  const { t } = useI18n();
  return (
    <section>
      <div className="flex items-center justify-between gap-3 mb-2">
        <h3 className="text-sm font-semibold text-gray-900">{title}</h3>
        <span className="text-xs text-gray-400">{entries.length}</span>
      </div>
      {entries.length === 0 ? (
        <p className="rounded-lg bg-gray-50 px-3 py-4 text-sm text-gray-500">{t('atif.snapshotNoChanges')}</p>
      ) : (
        <ul className="max-h-56 space-y-2 overflow-y-auto rounded-lg border border-gray-200 p-2">
          {entries.map((entry, index) => (
            <li key={`${entry.change_type}:${entry.path}:${entry.detail ?? ''}:${index}`} className="rounded-md bg-gray-50 px-3 py-2">
              <div className="flex items-start gap-2">
                <span className={`flex-shrink-0 rounded px-1.5 py-0.5 text-xs font-medium ${CHANGE_STYLES[entry.change_type]}`}>
                  {t(CHANGE_LABELS[entry.change_type])}
                </span>
                <span className="min-w-0 break-all font-mono text-xs text-gray-800">{entry.path}</span>
              </div>
              {entry.detail && <p className="mt-1 break-all font-mono text-xs text-gray-500">{entry.detail}</p>}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
};

export const WorkspaceSnapshotDialog: React.FC<WorkspaceSnapshotDialogProps> = ({ request, onClose }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const [detail, setDetail] = useState<WorkspaceSnapshotDetailResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [retryVersion, setRetryVersion] = useState(0);
  const requestVersion = useRef(0);
  const closeRef = useRef<HTMLButtonElement>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    const version = ++requestVersion.current;
    const controller = new AbortController();
    setDetail(null);
    setError('');
    if (!request) {
      setLoading(false);
      return () => controller.abort();
    }
    setLoading(true);
    fetchWorkspaceSnapshotDetail(request, controller.signal)
      .then(response => {
        if (requestVersion.current === version) setDetail(response);
      })
      .catch(nextError => {
        if (requestVersion.current === version && nextError?.name !== 'AbortError') {
          setError(errorMessage(nextError, t));
        }
      })
      .finally(() => {
        if (requestVersion.current === version) setLoading(false);
      });
    return () => {
      controller.abort();
      if (requestVersion.current === version) requestVersion.current += 1;
    };
  }, [request, retryVersion, t]);

  useEffect(() => {
    if (!request) return undefined;
    previousFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    closeRef.current?.focus();
    return () => {
      previousFocusRef.current?.focus();
      previousFocusRef.current = null;
    };
  }, [request]);

  if (!request) return null;

  const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') {
      onCloseRef.current();
      return;
    }
    if (event.key !== 'Tab') return;
    const focusable = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(
      'button:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
    ));
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (!first || !last) {
      event.preventDefault();
      return;
    }
    const focusOutside = !event.currentTarget.contains(document.activeElement);
    if (event.shiftKey && (document.activeElement === first || focusOutside)) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && (document.activeElement === last || focusOutside)) {
      event.preventDefault();
      first.focus();
    }
  };

  const snapshot = detail?.snapshot;
  return (
    <div
      data-testid="workspace-snapshot-backdrop"
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/50 p-4"
      onClick={event => {
        if (event.target === event.currentTarget) onCloseRef.current();
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="workspace-snapshot-title"
        tabIndex={-1}
        onKeyDown={handleKeyDown}
        className="max-h-[90vh] w-full max-w-xl overflow-y-auto rounded-2xl bg-white shadow-2xl outline-none"
      >
        <header className="flex items-start justify-between gap-4 border-b border-gray-200 px-6 py-5">
          <div className="min-w-0">
            <h2 id="workspace-snapshot-title" className="text-xl font-semibold text-gray-900">
              {t('atif.snapshotDialogTitle')}
            </h2>
            <p className="mt-1 break-all font-mono text-sm text-green-700">{snapshot?.id ?? request.snapshot_id}</p>
          </div>
          <button
            ref={closeRef}
            onClick={() => onCloseRef.current()}
            aria-label={t('atif.snapshotCloseAria')}
            className="rounded-lg px-2 py-1 text-gray-400 hover:bg-gray-100 hover:text-gray-700"
          >
            ×
          </button>
        </header>

        <div className="space-y-5 px-6 py-5">
          {loading && <p className="text-sm text-gray-500">{t('atif.loading')}</p>}
          {error && (
            <div className="rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-700">
              <p>{error}</p>
              <button onClick={() => setRetryVersion(version => version + 1)} className="mt-2 font-medium text-red-800 underline">
                {t('common.retry')}
              </button>
            </div>
          )}

          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm">
            <dt className="text-gray-500">{t('atif.snapshotWorkspace')}</dt>
            <dd className="break-all font-mono text-xs text-gray-800">{detail?.workspace.path ?? request.workspace.path}</dd>
            <dt className="text-gray-500">{t('atif.snapshotStepIndex')}</dt>
            <dd className="text-gray-800">{request.step_index}</dd>
            {snapshot && (
              <>
                <dt className="text-gray-500">{t('atif.snapshotCreated')}</dt>
                <dd className="text-gray-800">{formatTime(snapshot.meta.created_at, locale)}</dd>
                {snapshot.meta.parent_id && (
                  <>
                    <dt className="text-gray-500">{t('atif.snapshotParent')}</dt>
                    <dd className="break-all font-mono text-xs text-gray-800">{snapshot.meta.parent_id}</dd>
                  </>
                )}
              </>
            )}
          </dl>

          {snapshot?.meta.message && <p className="rounded-lg bg-gray-50 p-3 text-sm text-gray-700">{snapshot.meta.message}</p>}
          {(snapshot?.meta.pinned || snapshot?.meta.missing) && (
            <div className="flex gap-2 text-xs">
              {snapshot.meta.pinned && <span className="rounded bg-blue-50 px-2 py-1 text-blue-700">{t('atif.snapshotPinned')}</span>}
              {snapshot.meta.missing && <span className="rounded bg-amber-50 px-2 py-1 text-amber-700">{t('atif.snapshotMissing')}</span>}
            </div>
          )}

          <p className="rounded-lg border border-blue-100 bg-blue-50 p-3 text-xs text-blue-800">
            {t('atif.snapshotAssociationNote')}
          </p>

          {detail && (
            <>
              <DiffList title={t('atif.snapshotTargetToCurrent')} entries={detail.target_to_current} />
              <DiffList title={t('atif.snapshotCurrentToTarget')} entries={detail.current_to_target} />
            </>
          )}
        </div>

        <footer className="border-t border-gray-200 px-6 py-4">
          <p className="mb-3 text-xs leading-5 text-amber-700">{t('atif.rollbackDisabledExplanation')}</p>
          <div className="flex justify-end gap-2">
            <button onClick={() => onCloseRef.current()} className="rounded-lg bg-gray-100 px-4 py-2 text-sm text-gray-700 hover:bg-gray-200">
              {t('common.close')}
            </button>
            <button
              disabled
              title={t('atif.rollbackDisabledExplanation')}
              className="cursor-not-allowed rounded-lg bg-gray-200 px-4 py-2 text-sm font-medium text-gray-500"
            >
              {t('atif.confirmRollback')}
            </button>
          </div>
        </footer>
      </div>
    </div>
  );
};
