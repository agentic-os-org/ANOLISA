import type { SessionSavings } from './apiClient';

export type SavingsSortKey = 'total_input_tokens' | 'total_output_tokens'
  | 'compounded_saved' | 'compounded_savings_rate';

export interface SavingsSort {
  key: SavingsSortKey;
  direction: 'ascending' | 'descending';
}

/** Start a new metric descending, then cycle ascending and original order. */
export function nextSavingsSort(current: SavingsSort | null, key: SavingsSortKey): SavingsSort | null {
  if (current?.key !== key) return { key, direction: 'descending' };
  return current.direction === 'descending' ? { key, direction: 'ascending' } : null;
}

/** Preserve the query snapshot and its tie order while sorting displayed metrics. */
export function sortSavingsSessions(sessions: readonly SessionSavings[], sort: SavingsSort | null): SessionSavings[] {
  const result = [...sessions];
  if (sort) {
    const direction = sort.direction === 'ascending' ? 1 : -1;
    result.sort((a, b) => direction * (a[sort.key] - b[sort.key]));
  }
  return result;
}
