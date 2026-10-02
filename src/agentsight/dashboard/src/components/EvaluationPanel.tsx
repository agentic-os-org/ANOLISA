import React, { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import {
  EvaluationNotReadyError,
  EvaluationRef,
  EvaluationResult,
  evaluateConversation,
} from '../utils/apiClient';
import { EvaluationBadge } from './EvaluationBadge';
import { useI18n, interruptionTypeKey } from '../i18n';
import type { MessageKey } from '../i18n';

interface EvaluationPanelProps {
  conversationId: string;
  initialResult: EvaluationResult | null;
  onResult?: (result: EvaluationResult) => void;
}

export const EvaluationPanel: React.FC<EvaluationPanelProps> = ({
  conversationId,
  initialResult,
  onResult,
}) => {
  const navigate = useNavigate();
  const { t } = useI18n();
  const [result, setResult] = useState<EvaluationResult | null>(initialResult);
  const [expanded, setExpanded] = useState(false);
  const [loading, setLoading] = useState(false);
  const [pendingCount, setPendingCount] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setResult(initialResult);
  }, [initialResult]);

  const runEvaluation = async (force: boolean) => {
    setLoading(true);
    setError(null);
    try {
      const response = await evaluateConversation(conversationId, force);
      setResult(response.result);
      setPendingCount(null);
      onResult?.(response.result);
    } catch (err) {
      if (err instanceof EvaluationNotReadyError) {
        setPendingCount(err.pendingCallCount);
      } else {
        setError(err instanceof Error ? err.message : t('comp.eval.evaluationFailed'));
      }
    } finally {
      setLoading(false);
    }
  };

  const renderEvidenceLinks = (refs: EvaluationRef[]) => {
    if (refs.length === 0) return null;

    return (
      <div className="mt-1 flex flex-wrap gap-1">
        {refs.slice(0, 3).map((ref, index) => {
          const path = evidencePath(ref);
          return (
            <button
              key={`${ref.type}-${ref.id}-${ref.label}-${index}`}
              onClick={() => path && navigate(path)}
              disabled={!path}
              className="rounded border border-blue-200 bg-white px-1.5 py-0.5 text-[11px] text-blue-700 hover:bg-blue-50 disabled:cursor-not-allowed disabled:opacity-50"
              title={ref.id}
            >
              {evidenceLabel(ref.label, t)}
            </button>
          );
        })}
        {refs.length > 3 && <span className="text-[11px] text-gray-400">+{refs.length - 3}</span>}
      </div>
    );
  };

  return (
    <div className="border border-gray-200 bg-white rounded-lg p-3 text-sm">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-xs font-semibold text-gray-500">{t('comp.eval.qualityEvaluation')}</span>
            <EvaluationBadge result={result} />
          </div>
          {result ? (
            <div className="mt-2 space-y-1">
              <p className="text-sm text-gray-800">{summaryText(result, t)}</p>
              <p className="text-xs text-gray-500">
                {t('comp.eval.rootCausePrefix')} <span>{rootCauseLabel(result.root_cause, t)}</span>
              </p>
              <p className="text-xs text-gray-600">{recommendedActionText(result, t)}</p>
            </div>
          ) : (
            <p className="mt-2 text-xs text-gray-500">{t('comp.eval.noResult')}</p>
          )}
        </div>
        <button
          onClick={() => runEvaluation(false)}
          disabled={loading}
          className="px-3 py-1 rounded border border-blue-300 bg-blue-50 text-blue-700 text-xs font-medium hover:bg-blue-100 disabled:opacity-50"
        >
          {loading ? t('comp.eval.evaluating') : t('comp.eval.startEval')}
        </button>
      </div>

      {pendingCount !== null && (
        <div className="mt-3 flex items-center justify-between gap-3 rounded border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
          <span>{t('comp.eval.pendingCalls', { n: pendingCount })}</span>
          <button
            onClick={() => runEvaluation(true)}
            disabled={loading}
            className="rounded border border-amber-300 bg-white px-2 py-0.5 font-medium hover:bg-amber-100 disabled:opacity-50"
          >
            {t('comp.eval.forceEval')}
          </button>
        </div>
      )}

      {result?.metadata.evaluated_with_pending && (
        <div className="mt-3 rounded border border-amber-200 bg-amber-50 px-3 py-2 text-xs text-amber-800">
          {t('comp.eval.pendingWhenRan', { n: result.metadata.pending_call_count })}
        </div>
      )}

      {error && (
        <div className="mt-3 rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
          {error}
        </div>
      )}

      {result && (
        <div className="mt-3">
          <button
            onClick={() => setExpanded((value) => !value)}
            className="text-xs font-medium text-blue-700 hover:text-blue-900"
          >
            {expanded ? t('comp.eval.collapseDetails') : t('comp.eval.viewDetails')}
          </button>
          {expanded && (
            <div className="mt-2 grid gap-3 lg:grid-cols-2">
              <div>
                <h4 className="text-xs font-semibold text-gray-500">{t('comp.eval.dimensions')}</h4>
                <div className="mt-1 space-y-1">
                  {result.dimensions.map((dimension) => (
                    <div key={dimension.name} className="rounded bg-gray-50 px-2 py-1">
                      <div className="flex items-center justify-between gap-2">
                        <span className="text-xs text-gray-700">{dimensionLabel(dimension.name, t)}</span>
                        <span className="text-xs text-gray-500">
                          {Math.round(dimension.score * 100)}
                        </span>
                      </div>
                      <p className="mt-0.5 text-xs text-gray-500">{reasonText(dimension.reason)}</p>
                      {renderEvidenceLinks(dimension.evidence_refs)}
                    </div>
                  ))}
                </div>
              </div>
              <div>
                <h4 className="text-xs font-semibold text-gray-500">{t('comp.eval.findings')}</h4>
                <div className="mt-1 space-y-1">
                  {result.findings.length === 0 ? (
                    <p className="text-xs text-gray-400">{t('comp.eval.noFindings')}</p>
                  ) : (
                    result.findings.map((finding, index) => (
                      <div key={`${finding.code}-${finding.message}-${index}`} className="rounded bg-gray-50 px-2 py-1">
                        <div className="flex items-center justify-between gap-2">
                          <span className="text-xs text-gray-700" title={finding.code}>
                            {findingLabel(finding.code, t)}
                          </span>
                          <span className="text-xs text-gray-500">{severityLabel(finding.severity, t)}</span>
                        </div>
                        <p className="mt-0.5 text-xs text-gray-500">{findingMessageText(finding.message)}</p>
                        {renderEvidenceLinks(finding.evidence_refs)}
                      </div>
                    ))
                  )}
                </div>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
};

type TFunc = (key: MessageKey, params?: Record<string, string | number>) => string;

function evidencePath(ref: EvaluationRef): string | null {
  if (!ref.deeplink) return null;

  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(ref.deeplink.query ?? {})) {
    if (value !== null && value !== undefined) {
      params.set(key, String(value));
    }
  }
  const query = params.toString();
  return query ? `${ref.deeplink.route}?${query}` : ref.deeplink.route;
}

function summaryText(result: EvaluationResult, t: TFunc): string {
  if (result.verdict === 'pass') {
    return t('comp.eval.summary.pass');
  }
  if (result.verdict === 'warn') {
    return t('comp.eval.summary.warn', { cause: rootCauseLabel(result.root_cause, t) });
  }
  return t('comp.eval.summary.fail', { cause: rootCauseLabel(result.root_cause, t) });
}

const ACTION_KEY: Record<string, MessageKey> = {
  none: 'comp.eval.action.none',
  no_final_answer: 'comp.eval.action.no_final_answer',
  interrupted_main_call: 'comp.eval.action.interrupted_main_call',
  agent_crash: 'comp.eval.action.agent_crash',
  runtime_error: 'comp.eval.action.runtime_error',
  tool_failure: 'comp.eval.action.tool_failure',
  safety_risk: 'comp.eval.action.safety_risk',
  loop_detected: 'comp.eval.action.loop_detected',
  excessive_cost: 'comp.eval.action.excessive_cost',
  partial_snapshot: 'comp.eval.action.partial_snapshot',
};

function recommendedActionText(result: EvaluationResult, t: TFunc): string {
  if (result.verdict === 'pass') {
    return t('comp.eval.action.noActionRequired');
  }

  const actionKey = ACTION_KEY[result.root_cause];
  return actionKey ? t(actionKey) : (result.recommended_action ?? result.root_cause);
}

const ROOT_CAUSE_KEY: Record<string, MessageKey> = {
  none: 'comp.eval.cause.none',
  no_final_answer: 'comp.eval.cause.no_final_answer',
  interrupted_main_call: 'comp.eval.cause.interrupted_main_call',
  agent_crash: 'comp.eval.cause.agent_crash',
  runtime_error: 'comp.eval.cause.runtime_error',
  tool_failure: 'comp.eval.cause.tool_failure',
  safety_risk: 'comp.eval.cause.safety_risk',
  loop_detected: 'comp.eval.cause.loop_detected',
  excessive_cost: 'comp.eval.cause.excessive_cost',
  partial_snapshot: 'comp.eval.cause.partial_snapshot',
};

function rootCauseLabel(value: string, t: TFunc): string {
  const key = ROOT_CAUSE_KEY[value];
  return key ? t(key) : value;
}

const DIMENSION_KEY: Record<string, MessageKey> = {
  completion: 'comp.eval.dim.completion',
  runtime_health: 'comp.eval.dim.runtime_health',
  tool_use: 'comp.eval.dim.tool_use',
  efficiency: 'comp.eval.dim.efficiency',
  safety: 'comp.eval.dim.safety',
};

function dimensionLabel(value: string, t: TFunc): string {
  const key = DIMENSION_KEY[value];
  return key ? t(key) : value;
}

// reasonText and findingMessageText are identity-mapping pass-throughs for
// backend-provided English reason/message strings.
function reasonText(value: string): string {
  return value;
}

const FINDING_KEY: Record<string, MessageKey> = {
  no_final_answer: 'comp.eval.finding.no_final_answer',
  interrupted_main_call: 'comp.eval.finding.interrupted_main_call',
  partial_snapshot: 'comp.eval.finding.partial_snapshot',
  tool_failure: 'comp.eval.finding.tool_failure',
  loop_detected: 'comp.eval.finding.loop_detected',
  llm_error: 'comp.eval.finding.llm_error',
  sse_truncated: 'comp.eval.finding.sse_truncated',
  network_timeout: 'comp.eval.finding.network_timeout',
  service_unavailable: 'comp.eval.finding.service_unavailable',
  agent_crash: 'comp.eval.finding.agent_crash',
};

function findingLabel(value: string, t: TFunc): string {
  const key = FINDING_KEY[value];
  if (key) return t(key);
  const typeKey = interruptionTypeKey(value);
  return typeKey ? t(typeKey) : value;
}

function findingMessageText(value: string): string {
  return value;
}

const SEVERITY_KEY: Record<string, MessageKey> = {
  critical: 'common.critical',
  high: 'common.high',
  medium: 'common.medium',
  low: 'common.low',
};

function severityLabel(value: string, t: TFunc): string {
  const key = SEVERITY_KEY[value];
  return key ? t(key) : value;
}

function evidenceLabel(value: string, t: TFunc): string {
  return findingLabel(value, t);
}
