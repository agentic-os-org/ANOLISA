// ATIF trajectory round model.
//
// Pure algorithms for the round-based trajectory view in the ATIF viewer:
// round grouping, initial (highlighted/default) round selection and
// per-round statistics. Extracted from AtifViewerPage so they can be
// exercised directly without reconstructing the whole page; the viewer
// keeps ownership of fetching, importing, navigation and rendering.
//
// Everything here is pure — no React, no fetching. Inputs are never
// mutated: grouping builds its own round/step arrays, and statistics are
// computed read-only.

import type { AtifStep, AtifToolCall } from '../types';
import type { MessageKey } from '../i18n';

/** Translator matching the i18n `t` function used by the viewer. */
export type Translate = (key: MessageKey, params?: Record<string, string | number>) => string;

export interface Round {
  key: number;
  label: string;
  /** True for the synthetic leading round that only carries the system prompt.
   *  Kept separate from `label` so consumers never branch on translated text. */
  isPreamble: boolean;
  userStep: AtifStep | null;
  steps: AtifStep[];
}

export interface RoundStats {
  toolCallCount: number;
  promptSum: number;
  completionSum: number;
  firstTs?: string;
  preview: string;
}

function toolCallsOf(step: AtifStep): AtifToolCall[] {
  return Array.isArray(step.tool_calls) ? step.tool_calls : [];
}

/** Group steps into rounds: a round starts at each user step and spans the
 *  following agent/system steps, mirroring the round-based trajectory view. */
export function groupIntoRounds(steps: AtifStep[], t: Translate): Round[] {
  const rounds: Round[] = [];
  let userRoundCount = 0;
  for (const step of steps) {
    if (step.source === 'user' || rounds.length === 0) {
      const isUser = step.source === 'user';
      if (isUser) userRoundCount++;
      rounds.push({
        key: rounds.length,
        label: isUser ? t('atif.round', { n: userRoundCount }) : t('atif.preamble'),
        isPreamble: !isUser,
        userStep: isUser ? step : null,
        steps: [step],
      });
    } else {
      rounds[rounds.length - 1].steps.push(step);
    }
  }
  return rounds;
}

/** Round to auto-select: prefer the highlighted round, else the first round. */
export function initialRound(rounds: Round[], sections: Set<string>): number | null {
  if (rounds.length === 0) return null;
  if (sections.size > 0) {
    const stepIds = new Set<number>();
    sections.forEach(k => stepIds.add(parseInt(k, 10)));
    for (const round of rounds) {
      if (round.steps.some(s => stepIds.has(s.step_id))) return round.key;
    }
  }
  return rounds[0].key;
}

/** Aggregate per-round statistics: tool-call and token totals, the round's
 *  first timestamp and a whitespace-collapsed preview of its opening
 *  message (the user step's message, else the first step carrying one). */
export function roundStats(round: Round): RoundStats {
  let toolCallCount = 0, promptSum = 0, completionSum = 0;
  for (const s of round.steps) {
    toolCallCount += toolCallsOf(s).length;
    promptSum += s.metrics?.prompt_tokens ?? 0;
    completionSum += s.metrics?.completion_tokens ?? 0;
  }
  const preview = (round.userStep?.message ?? round.steps.find(s => s.message)?.message ?? '')
    .replace(/\s+/g, ' ')
    .trim();
  return { toolCallCount, promptSum, completionSum, firstTs: round.steps.find(s => s.timestamp)?.timestamp, preview };
}
