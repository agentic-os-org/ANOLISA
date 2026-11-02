import React, { useEffect, useRef, useState } from 'react';
import { useI18n, useLocaleTag } from '../i18n';
import { ApiRequestError } from '../utils/apiClient';
import {
  executeSnapshotRestore, fetchSnapshotDiff, fetchSnapshotRestoreResult, prepareSnapshotRestore,
} from '../utils/wsCkpt';
import type { FileChange, RestoreConfirmation, SnapshotDiff, SnapshotScope, WorkspaceSnapshot } from '../utils/wsCkpt';

export interface SnapshotSelection {
  snapshot: WorkspaceSnapshot;
  scope: SnapshotScope;
  restoreEnabled: boolean;
}

export type RestoreAttention = 'pending' | 'failed';

const CHANGE_STYLE = {
  Added: 'bg-green-100 text-green-700', Modified: 'bg-amber-100 text-amber-700',
  Deleted: 'bg-red-100 text-red-700', Renamed: 'bg-blue-100 text-blue-700',
};
const CHANGE_LETTER = { Added: 'A', Modified: 'M', Deleted: 'D', Renamed: 'R' };
const IMPACT_KEY = {
  Added: 'snapshot.deleteFile', Modified: 'snapshot.revertFile',
  Deleted: 'snapshot.restoreFile', Renamed: 'snapshot.revertRename',
} as const;
const FILES_PER_PAGE = 20;
const RESULT_RETRY_DELAYS_MS = [1000, 2000];

export const WorkspaceSnapshotDialog: React.FC<{
  selection: SnapshotSelection;
  open: boolean;
  restoreBlocked?: boolean;
  onClose: () => void;
  onRestoreAttentionChange: (attention: RestoreAttention | null) => void;
}> = ({ selection, open, restoreBlocked, onClose, onRestoreAttentionChange }) => {
  const { t } = useI18n();
  const locale = useLocaleTag();
  const { snapshot, scope, restoreEnabled } = selection;
  const target = { workspace: scope.ws_id, snapshot_id: snapshot.id };
  const [diff, setDiff] = useState<SnapshotDiff | null>(null);
  const [filePage, setFilePage] = useState(0);
  const [busy, setBusy] = useState(false);
  const [stopped, setStopped] = useState(false);
  const [error, setError] = useState('');
  const [confirmation, setConfirmation] = useState<RestoreConfirmation | null>(null);
  const [restoreStatus, setRestoreStatus] = useState<'failed' | 'succeeded' | 'querySucceeded' | null>(null);
  const [checkingResult, setCheckingResult] = useState(false);
  const succeeded = restoreStatus === 'succeeded' || restoreStatus === 'querySucceeded';
  const failed = restoreStatus === 'failed';
  const busyRef = useRef(false);
  const dialogRef = useRef<HTMLDivElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);

  const errorText = (value: unknown) => value instanceof ApiRequestError && typeof value.body?.error === 'string'
    ? value.body.error : t('snapshot.requestFailed');

  const loadDiff = async () => {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    setDiff(null);
    setFilePage(0);
    setStopped(false);
    if (!failed) setError('');
    try {
      const result = await fetchSnapshotDiff(target);
      if (result.scope.ws_id !== scope.ws_id || result.scope.path !== scope.path || result.snapshot_id !== snapshot.id) {
        throw new Error('Snapshot target changed');
      }
      setDiff(result);
    } catch (e) { setError(errorText(e)); }
    finally { busyRef.current = false; setBusy(false); }
  };

  // The parent keys this dialog by the fixed selection, never the active round.
  useEffect(() => { void loadDiff(); }, []);
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    dialogRef.current?.focus();
    const overflow = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    return () => { document.body.style.overflow = overflow; previous?.focus(); };
  }, [open]);

  const restore = async () => {
    if (busyRef.current || !diff?.restore_preview || !stopped || !restoreEnabled || restoreBlocked || confirmation || succeeded) return;
    busyRef.current = true;
    setBusy(true);
    setError('');
    let prepared: RestoreConfirmation | null = null;
    try {
      prepared = await prepareSnapshotRestore(target, diff.restore_preview);
      setConfirmation(prepared);
      setRestoreStatus(null);
      onRestoreAttentionChange('pending');
      const result = await executeSnapshotRestore(prepared.confirmation_token);
      if (result.status === 'succeeded') {
        setRestoreStatus('succeeded');
        onRestoreAttentionChange(null);
      }
    } catch (e) {
      if (!prepared || (e instanceof ApiRequestError && e.body?.effect === 'known_no_effect')) {
        setConfirmation(null);
        setStopped(false);
        setRestoreStatus('failed');
        onRestoreAttentionChange('failed');
        if (!prepared && e instanceof ApiRequestError && e.status === 409) setDiff(null);
      }
      setError(!prepared && e instanceof ApiRequestError && e.status === 409
        ? t('snapshot.stalePreview') : errorText(e));
    } finally { busyRef.current = false; setBusy(false); }
  };

  const cancelRestore = () => {
    if (busyRef.current || !failed) return;
    setRestoreStatus(null);
    setError('');
    onRestoreAttentionChange(null);
    onClose();
  };

  useEffect(() => {
    if (!open || !confirmation || busy || succeeded) return;
    const controller = new AbortController();
    const { confirmation_token } = confirmation;
    let retry: ReturnType<typeof setTimeout> | undefined;
    setCheckingResult(true);
    const check = async (attempt: number) => {
      let failure: unknown;
      try {
        const result = await fetchSnapshotRestoreResult(confirmation_token, controller.signal);
        if (controller.signal.aborted) return;
        const outcome = result.evidence?.outcome;
        if (outcome && typeof outcome === 'object' && 'Succeeded' in outcome) {
          setError('');
          setCheckingResult(false);
          setRestoreStatus('querySucceeded');
          onRestoreAttentionChange(null);
          return;
        }
      } catch (e) {
        if (controller.signal.aborted) return;
        failure = e;
      }
      const delay = RESULT_RETRY_DELAYS_MS[attempt];
      if (delay !== undefined && !(failure instanceof ApiRequestError && failure.status === 401)) {
        retry = setTimeout(() => void check(attempt + 1), delay);
      } else {
        if (failure) setError(errorText(failure));
        setCheckingResult(false);
      }
    };
    void check(0);
    return () => {
      controller.abort();
      clearTimeout(retry);
    };
  }, [open, confirmation, busy, succeeded]);

  const changes = diff?.changes ?? [];
  const pageCount = Math.ceil(changes.length / FILES_PER_PAGE);
  const firstFile = filePage * FILES_PER_PAGE;
  const canRestore = restoreEnabled && !!diff?.restore_preview;
  const restoreControls = canRestore && !confirmation && !succeeded && (restoreBlocked
    ? <p className="text-sm text-amber-800">{t('snapshot.restoreBlocked')}</p>
    : <label className="flex items-start gap-2 text-sm text-gray-700">
        <input type="checkbox" checked={stopped} disabled={busy} onChange={e => setStopped(e.target.checked)} className="mt-1" />
        {t('snapshot.stopped')}
      </label>);
  const showFilePage = (page: number) => {
    setFilePage(page);
    if (bodyRef.current) bodyRef.current.scrollTop = 0;
  };
  const counts = changes.reduce((result, change) => {
    result[change.change_type] = (result[change.change_type] ?? 0) + 1;
    return result;
  }, {} as Partial<Record<FileChange['change_type'], number>>);

  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/40 p-4" onClick={e => {
      if (e.target === e.currentTarget && !busy) onClose();
    }}>
      <div ref={dialogRef} tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby="snapshot-title"
        className="flex max-h-[90vh] w-full max-w-xl flex-col rounded-2xl bg-white shadow-2xl outline-none"
        onKeyDown={e => {
          if (e.key === 'Escape' && !busy) onClose();
          if (e.key !== 'Tab') return;
          const items = Array.from(e.currentTarget.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled])'));
          const first = items[0], last = items[items.length - 1];
          const fromDialog = document.activeElement === e.currentTarget;
          if (e.shiftKey && (document.activeElement === first || fromDialog)) { e.preventDefault(); last?.focus(); }
          else if (!e.shiftKey && (document.activeElement === last || fromDialog)) { e.preventDefault(); first?.focus(); }
        }}>
        <header className="flex items-start justify-between gap-4 border-b border-gray-100 px-6 py-5">
          <div className="min-w-0">
            <h2 id="snapshot-title" className="break-words text-lg font-semibold text-gray-900">
              {t('snapshot.name', { name: snapshot.meta.message || snapshot.id })}
            </h2>
            <p className="mt-1 text-sm text-gray-500">{new Date(snapshot.meta.created_at).toLocaleString(locale)}</p>
          </div>
          <button type="button" aria-label={t('snapshot.close')} disabled={busy} onClick={onClose}
            className="rounded-lg px-2 text-2xl text-gray-400 hover:bg-gray-100 disabled:opacity-40">×</button>
        </header>
        <div ref={bodyRef} className="min-h-0 space-y-4 overflow-y-auto px-6 py-4">
          <div className="text-sm text-gray-600">
            <p className="break-all font-mono">{scope.path}</p>
            <p className="mt-2 break-all text-xs text-gray-500">ID: {snapshot.id}<br />ws_id: {scope.ws_id}</p>
            <p className="mt-2 text-xs">{t('snapshot.association')}</p>
          </div>
          {!succeeded && <>
            <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-gray-500">
              <span>{t('snapshot.direction')}</span>
              <button type="button" disabled={busy || !!confirmation} onClick={() => void loadDiff()} className="text-blue-600 disabled:opacity-40">{t('snapshot.refresh')}</button>
            </div>
            {busy && <p role="status" className="text-sm text-gray-500">{t('snapshot.working')}</p>}
            {diff && <>
              <p className="text-xs text-gray-400">{t('snapshot.comparedAt', { time: new Date(diff.compared_at).toLocaleString(locale) })}</p>
              <ul className="divide-y divide-gray-100 rounded-xl border border-gray-200">
                {changes.slice(firstFile, firstFile + FILES_PER_PAGE).map((change, i) => <li key={firstFile + i} className="flex items-start gap-3 px-3 py-3 text-sm">
                  <span className={`rounded px-1.5 py-0.5 text-xs font-semibold ${CHANGE_STYLE[change.change_type]}`}>{CHANGE_LETTER[change.change_type]}</span>
                  <span className="min-w-0 flex-1 break-all font-mono text-gray-700">{change.path}{change.detail && <small className="block font-sans text-gray-500">{change.detail}</small>}</span>
                  <span className="shrink-0 text-xs text-gray-500">{t(IMPACT_KEY[change.change_type])}</span>
                </li>)}
                {diff.changes.length === 0 && <li className="p-4 text-sm text-gray-500">{t('snapshot.noChanges')}</li>}
              </ul>
              <section className="rounded-xl bg-gray-50 p-4 text-sm text-gray-700">
                <h3 className="font-semibold">{t('snapshot.impactTitle')}</h3>
                <div className="mt-2 flex flex-wrap gap-3">
                  {Object.entries(counts).map(([kind, n]) => <span key={kind}>{t(IMPACT_KEY[kind as FileChange['change_type']])} {n}</span>)}
                </div>
                <p className="mt-2 text-xs text-gray-500">{t('snapshot.wholeWorkspace')}</p>
              </section>
            </>}
            {(!restoreEnabled || (diff && !canRestore)) && <p className="text-sm text-gray-500">{t('snapshot.disabled')}</p>}
            {pageCount <= 1 && restoreControls}
          </>}
          {confirmation && <div role="status" className={`rounded-xl border p-4 text-sm ${succeeded ? 'border-green-200 bg-green-50 text-green-800' : 'border-red-300 bg-red-50 text-red-900'}`}>
            <p className="font-medium">{t(succeeded ? `snapshot.${restoreStatus}` : checkingResult ? 'snapshot.checkingResult' : 'snapshot.unknown')}</p>
            <p className="mt-1 break-all text-xs">{t('snapshot.operation', { id: confirmation.operation_id })}</p>
          </div>}
          {failed ? <div role="alert" className="space-y-1 break-words rounded-xl border border-red-300 bg-red-50 p-4 text-sm text-red-900">
            <p className="font-medium">{t('snapshot.restoreFailed')}</p>
            {error && <p>{error}</p>}
          </div> : error && <p role="alert" className="break-words rounded-lg bg-red-50 p-3 text-sm text-red-700">{error}</p>}
        </div>
        <footer className="shrink-0 space-y-3 border-t border-gray-100 px-6 py-4">
          {pageCount > 1 && !succeeded && <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-gray-600">
            <span>{t('snapshot.fileCount', { n: changes.length })}</span>
            <div className="flex items-center gap-2">
              <button type="button" disabled={filePage === 0} onClick={() => showFilePage(filePage - 1)}
                className="rounded border px-2 py-1 disabled:opacity-40">{t('common.prev')}</button>
              <span aria-live="polite">{t('common.page', { cur: filePage + 1, total: pageCount })}</span>
              <button type="button" disabled={filePage + 1 === pageCount} onClick={() => showFilePage(filePage + 1)}
                className="rounded border px-2 py-1 disabled:opacity-40">{t('common.next')}</button>
            </div>
          </div>}
          {pageCount > 1 && restoreControls}
          <div className="flex flex-wrap justify-end gap-3">
            <button type="button" disabled={busy} onClick={onClose} className="rounded-lg border border-gray-300 px-4 py-2 text-sm disabled:opacity-40">{t('snapshot.close')}</button>
            {!succeeded && <>
              {(failed || confirmation) && <button type="button" disabled={busy || !failed} onClick={cancelRestore}
                className="rounded-lg border border-gray-300 px-4 py-2 text-sm disabled:opacity-40">{t('snapshot.cancelRestore')}</button>}
              <button type="button" disabled={busy || !!confirmation || !changes.length || !stopped || !canRestore || restoreBlocked} onClick={() => void restore()}
                className="rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white disabled:opacity-40">{t('snapshot.confirm')}</button>
            </>}
          </div>
        </footer>
      </div>
    </div>
  );
};
