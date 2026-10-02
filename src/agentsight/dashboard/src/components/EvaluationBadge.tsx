import React from 'react';
import { EvaluationResult } from '../utils/apiClient';
import { useI18n } from '../i18n';
import type { MessageKey } from '../i18n';

interface EvaluationBadgeProps {
  result: Pick<EvaluationResult, 'verdict' | 'score'> | null;
}

const STYLE_BY_VERDICT = {
  pass: 'bg-green-50 text-green-700 border-green-200',
  warn: 'bg-amber-50 text-amber-700 border-amber-200',
  fail: 'bg-red-50 text-red-700 border-red-200',
} as const;

const VERDICT_LABEL_KEY: Record<string, MessageKey> = {
  pass: 'comp.eval.pass',
  warn: 'comp.eval.review',
  fail: 'comp.eval.fail',
};

export const EvaluationBadge: React.FC<EvaluationBadgeProps> = ({ result }) => {
  const { t } = useI18n();
  if (!result) return null;

  const style =
    STYLE_BY_VERDICT[result.verdict as keyof typeof STYLE_BY_VERDICT] ??
    'bg-gray-50 text-gray-700 border-gray-200';
  const labelKey = VERDICT_LABEL_KEY[result.verdict];
  const label = labelKey ? t(labelKey) : result.verdict;

  return (
    <span
      className={`inline-flex items-center gap-1 rounded border px-2 py-0.5 text-xs font-semibold ${style}`}
      title={t('comp.eval.qualityScore', { n: Math.round(result.score * 100) })}
    >
      <span>{label}</span>
      <span className="font-mono">{Math.round(result.score * 100)}</span>
    </span>
  );
};
