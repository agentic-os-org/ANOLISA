import React, { useEffect, useRef, useState } from 'react';
import {
  containmentTargetCandidates,
  containSecurityCase,
  defaultContainmentTargetPid,
  enforcementSupportsContainment,
  fetchEnforcementHealth,
  fetchContainmentPlan,
  SecurityApiClientError,
  type EnforcementHealth,
  type SecurityContainmentAction,
  type SecurityContainmentPlan,
} from '../utils/apiClient';
import { useI18n, useLocaleTag } from '../i18n';
import type { MessageKey } from '../i18n';

type DurationMode = 'temporary' | 'persistent';

export interface ContainmentDialogProps {
  caseId: string;
  open: boolean;
  onClose: () => void;
  onContained: (action: SecurityContainmentAction) => void;
}

const containmentErrorMessages: Record<string, MessageKey> = {
  source_policy_unavailable: 'cont.error.sourcePolicyUnavailable',
  root_process_stale: 'cont.error.rootProcessStale',
  ambiguous_candidate: 'cont.error.ambiguousCandidate',
  case_not_eligible: 'cont.error.caseNotEligible',
  case_eligibility_changed: 'cont.error.caseEligibilityChanged',
  invalid_duration: 'cont.error.invalidDuration',
  incompatible_action: 'cont.error.incompatibleAction',
  action_in_progress: 'cont.error.actionInProgress',
  action_expiring: 'cont.error.actionExpiring',
  cleanup_required: 'cont.error.cleanupRequired',
  enforcer_unavailable: 'cont.error.enforcerUnavailable',
  containment_disabled: 'cont.error.containmentDisabled',
  recovery_failed: 'cont.error.recoveryFailed',
  health_store_unavailable: 'cont.error.healthStoreUnavailable',
};

function safeErrorMessage(
  error: unknown,
  t: (key: MessageKey) => string,
): string {
  if (error instanceof SecurityApiClientError) {
    const key = containmentErrorMessages[error.code];
    if (key) return t(key);
    return error.retryable ? t('cont.error.retryable') : t('cont.error.nonRetryable');
  }
  return t('cont.error.generic');
}

function existingActionNotice(action: SecurityContainmentAction): {
  titleKey: MessageKey;
  messageKey: MessageKey;
  live: boolean;
} {
  switch (action.lifecycle_state) {
    case 'pending':
      return {
        titleKey: 'cont.existing.pending.title',
        messageKey: 'cont.existing.pending.message',
        live: true,
      };
    case 'active':
      return action.duration_secs === null
        ? {
            titleKey: 'cont.existing.activePersistent.title',
            messageKey: 'cont.existing.activePersistent.message',
            live: true,
          }
        : {
            titleKey: 'cont.existing.activeTemporary.title',
            messageKey: 'cont.existing.activeTemporary.message',
            live: true,
          };
    case 'expiring':
      return {
        titleKey: 'cont.existing.expiring.title',
        messageKey: 'cont.existing.expiring.message',
        live: true,
      };
    case 'expired':
      return {
        titleKey: 'cont.existing.expired.title',
        messageKey: 'cont.existing.expired.message',
        live: false,
      };
    case 'failed':
      return {
        titleKey: 'cont.existing.failed.title',
        messageKey: 'cont.existing.failed.message',
        live: false,
      };
  }
}

export const ContainmentDialog: React.FC<ContainmentDialogProps> = ({
  caseId,
  open,
  onClose,
  onContained,
}) => {
  const { t } = useI18n();
  const localeTag = useLocaleTag();
  void localeTag;

  const [plan, setPlan] = useState<SecurityContainmentPlan | null>(null);
  const [selectedPid, setSelectedPid] = useState<number | null>(null);
  const [enforcementHealth, setEnforcementHealth] = useState<EnforcementHealth | null>(null);
  const [durationMode, setDurationMode] = useState<DurationMode>('temporary');
  const [loading, setLoading] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const requestVersion = useRef(0);
  const dialogRef = useRef<HTMLDivElement>(null);
  const headerCloseRef = useRef<HTMLButtonElement>(null);
  const firstDecisionRef = useRef<HTMLElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;

  useEffect(() => {
    requestVersion.current += 1;
    const version = requestVersion.current;
    setPlan(null);
    setSelectedPid(null);
    setEnforcementHealth(null);
    setDurationMode('temporary');
    setSubmitting(false);
    setError('');
    if (!open) {
      setLoading(false);
      return undefined;
    }

    setLoading(true);
    void Promise.all([fetchContainmentPlan(caseId), fetchEnforcementHealth()])
      .then(([response, health]) => {
        if (requestVersion.current !== version) return;
        const nextPlan = response.data;
        setPlan(nextPlan);
        setEnforcementHealth(health);
        setSelectedPid(defaultContainmentTargetPid(nextPlan, health));
      })
      .catch((nextError) => {
        if (requestVersion.current === version) setError(safeErrorMessage(nextError, t));
      })
      .finally(() => {
        if (requestVersion.current === version) setLoading(false);
      });
    return () => {
      if (requestVersion.current === version) requestVersion.current += 1;
    };
  }, [caseId, open, t]);

  useEffect(() => {
    if (!open) return undefined;
    previousFocusRef.current = document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
    return () => {
      previousFocusRef.current?.focus();
      previousFocusRef.current = null;
    };
  }, [open]);

  const existingNotice = plan?.existing_action
    ? existingActionNotice(plan.existing_action)
    : null;
  const liveExistingAction = Boolean(existingNotice?.live);
  const containmentAvailable = enforcementSupportsContainment(enforcementHealth);
  const containmentUnavailableMessage = enforcementHealth === null
    ? t('cont.unavailable.loading')
    : !enforcementHealth.ready
      ? t('cont.unavailable.notReady')
      : t('cont.unavailable.noCredentialEnforce');

  useEffect(() => {
    if (!open) return;
    if (plan && !liveExistingAction) {
      firstDecisionRef.current?.focus();
    } else {
      headerCloseRef.current?.focus();
    }
  }, [liveExistingAction, open, plan]);

  if (!open) return null;

  const targetCandidates = plan
    ? containmentTargetCandidates(plan, enforcementHealth)
    : [];
  const canSubmit = Boolean(
    plan
      && selectedPid !== null
      && targetCandidates.some((candidate) => candidate.root_pid === selectedPid)
      && containmentAvailable
      && !loading
      && !submitting
      && !liveExistingAction,
  );
  const originalTarget = plan?.original_target;
  const targetStale = Boolean(plan && !plan.original_target_valid);
  const durationMinutes = Math.round((plan?.default_duration_secs ?? 900) / 60);

  const handleDialogKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') {
      if (!submitting) onCloseRef.current();
      return;
    }
    if (event.key !== 'Tab') return;

    const focusable = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])',
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

  const submit = async () => {
    if (
      !plan
      || selectedPid === null
      || !targetCandidates.some((candidate) => candidate.root_pid === selectedPid)
      || !containmentAvailable
      || loading
      || submitting
    ) return;
    const version = requestVersion.current;
    setSubmitting(true);
    setError('');
    try {
      const response = await containSecurityCase(caseId, {
        root_pid: selectedPid,
        duration_secs: durationMode === 'temporary' ? plan.default_duration_secs : null,
      });
      if (requestVersion.current === version) onContained(response.data);
    } catch (nextError) {
      if (requestVersion.current === version) setError(safeErrorMessage(nextError, t));
    } finally {
      if (requestVersion.current === version) setSubmitting(false);
    }
  };

  return (
    <div
      data-testid="containment-backdrop"
      className="fixed inset-0 z-50 flex items-center justify-center bg-slate-950/50 p-4"
      onClick={(event) => {
        if (event.target === event.currentTarget && !submitting) onCloseRef.current();
      }}
    >
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby="containment-dialog-title"
        tabIndex={-1}
        onKeyDown={handleDialogKeyDown}
        className="max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-2xl bg-white shadow-2xl outline-none"
      >
        <header className="flex items-start justify-between border-b border-gray-200 px-6 py-5">
          <div>
            <h2 id="containment-dialog-title" className="text-xl font-semibold text-gray-900">
              {t(containmentAvailable ? 'cont.dialog.title.available' : 'cont.dialog.title.unavailable')}
            </h2>
            <p className="mt-1 text-sm text-gray-500">
              {t('cont.dialog.subtitle')}
            </p>
          </div>
          <button
            ref={headerCloseRef}
            type="button"
            aria-label={t('cont.dialog.closeAria')}
            onClick={onClose}
            disabled={submitting}
            className="rounded-lg px-2 py-1 text-xl text-gray-400 hover:bg-gray-100 hover:text-gray-700 disabled:opacity-40"
          >
            ×
          </button>
        </header>

        <div className="space-y-5 px-6 py-5">
          {loading && (
            <p role="status" className="text-sm text-gray-500">
              {t('cont.plan.loading')}
            </p>
          )}

          {plan && (
            <>
              <dl className="grid gap-3 rounded-xl border border-gray-200 bg-gray-50 p-4 sm:grid-cols-2">
                <div>
                  <dt className="text-xs font-medium text-gray-500">{t('cont.plan.field.sourcePath')}</dt>
                  <dd className="mt-1 break-all font-mono text-xs font-medium text-gray-900">
                    {plan.source_path}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs font-medium text-gray-500">{t('cont.plan.field.scope')}</dt>
                  <dd className="mt-1 text-sm font-medium text-gray-900">
                    {t('cont.plan.scope.untrustedIpv4')}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs font-medium text-gray-500">{t('cont.plan.field.effect')}</dt>
                  <dd className="mt-1 text-sm font-medium text-gray-900">
                    {t('cont.plan.effect.kernelDeny')}
                  </dd>
                </div>
                <div>
                  <dt className="text-xs font-medium text-gray-500">{t('cont.plan.field.originalAgent')}</dt>
                  <dd className="mt-1 text-sm font-medium text-gray-900">
                    {originalTarget
                      ? `${originalTarget.display_name} · PID ${originalTarget.root_pid}`
                      : t('cont.plan.original.unavailable')}
                  </dd>
                </div>
              </dl>

              {existingNotice && (
                <div
                  role="status"
                  className={`rounded-xl border px-4 py-3 ${
                    existingNotice.live
                      ? 'border-blue-200 bg-blue-50 text-blue-800'
                      : 'border-amber-200 bg-amber-50 text-amber-800'
                  }`}
                >
                  <p className="text-sm font-semibold">{t(existingNotice.titleKey)}</p>
                  <p className="mt-1 text-xs">{t(existingNotice.messageKey)}</p>
                </div>
              )}

              {!liveExistingAction && (!targetStale && originalTarget ? (
                <p className="rounded-lg bg-emerald-50 px-3 py-2 text-sm text-emerald-700">
                  {t('cont.target.valid', { pid: originalTarget.root_pid })}
                </p>
              ) : targetCandidates.length > 0 ? (
                <div className="space-y-2 rounded-xl border border-amber-200 bg-amber-50 p-4">
                  <label htmlFor="containment-target" className="block text-sm font-medium text-amber-900">
                    {t('cont.target.selectLabel')}
                  </label>
                  <select
                    id="containment-target"
                    value={selectedPid ?? ''}
                    onChange={(event) => {
                      setSelectedPid(event.target.value === '' ? null : Number(event.target.value));
                    }}
                    className="w-full rounded-lg border border-amber-300 bg-white px-3 py-2 text-sm text-gray-900"
                  >
                    <option value="">{t('cont.target.placeholder')}</option>
                    {targetCandidates.map((candidate) => (
                      <option
                        key={`${candidate.root_pid}:${candidate.process_start_time}`}
                        value={candidate.root_pid}
                      >
                        {candidate.display_name} · PID {candidate.root_pid}
                      </option>
                    ))}
                  </select>
                  <p className="text-xs text-amber-800">
                    {t('cont.target.recheckHint')}
                  </p>
                </div>
              ) : (
                <p className="rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">
                  {t('cont.target.noneAvailable')}
                </p>
              ))}

              {!containmentAvailable && (
                <p className="rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">
                  {containmentUnavailableMessage}
                </p>
              )}

              {!liveExistingAction && (
                <fieldset className="space-y-3">
                  <legend className="text-sm font-medium text-gray-800">
                    {t('cont.duration.legend')}
                  </legend>
                  <label className="flex cursor-pointer items-start gap-3 rounded-xl border border-gray-200 p-4">
                    <input
                      ref={(element) => {
                        firstDecisionRef.current = element;
                      }}
                      type="radio"
                      name="containment-duration"
                      aria-label={t('cont.duration.temporaryAria', { minutes: durationMinutes })}
                      checked={durationMode === 'temporary'}
                      onChange={() => setDurationMode('temporary')}
                      className="mt-1"
                    />
                    <span>
                      <span className="block text-sm font-medium text-gray-900">
                        {t('cont.duration.temporaryTitle', { minutes: durationMinutes })}
                      </span>
                      <span className="mt-1 block text-xs text-gray-500">
                        {t('cont.duration.temporaryDesc')}
                      </span>
                    </span>
                  </label>
                  <label className="flex cursor-pointer items-start gap-3 rounded-xl border border-gray-200 p-4">
                    <input
                      type="radio"
                      name="containment-duration"
                      aria-label={t('cont.duration.persistentAria')}
                      checked={durationMode === 'persistent'}
                      onChange={() => setDurationMode('persistent')}
                      className="mt-1"
                    />
                    <span>
                      <span className="block text-sm font-medium text-gray-900">
                        {t('cont.duration.persistentTitle')}
                      </span>
                      <span className="mt-1 block text-xs text-gray-500">
                        {t('cont.duration.persistentDesc')}
                      </span>
                    </span>
                  </label>
                </fieldset>
              )}
            </>
          )}

          {error && (
            <p
              role="alert"
              className="rounded-lg border border-red-200 bg-red-50 px-4 py-3 text-sm text-red-700"
            >
              {error}
            </p>
          )}
        </div>

        <footer className="flex justify-end gap-3 border-t border-gray-200 px-6 py-4">
          <button
            type="button"
            onClick={onClose}
            disabled={submitting}
            className="rounded-lg border border-gray-300 bg-white px-4 py-2 text-sm text-gray-700 disabled:opacity-40"
          >
            {liveExistingAction ? t('cont.footer.close') : t('cont.footer.cancel')}
          </button>
          {!liveExistingAction && (
            <button
              type="button"
              onClick={() => void submit()}
              disabled={!canSubmit}
              className="rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-700 disabled:bg-red-300"
            >
              {submitting ? t('cont.footer.submit.loading') : t('cont.footer.submit')}
            </button>
          )}
        </footer>
      </div>
    </div>
  );
};
