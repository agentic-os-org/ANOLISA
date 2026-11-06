import type { SessionSavings } from './apiClient';
import { compoundedSavingsRate } from './savings';

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
    result.sort((a, b) => {
      const aValue = sort.key === 'compounded_savings_rate'
        ? compoundedSavingsRate(a.compounded_saved, a.baseline_tokens)
        : a[sort.key];
      const bValue = sort.key === 'compounded_savings_rate'
        ? compoundedSavingsRate(b.compounded_saved, b.baseline_tokens)
        : b[sort.key];
      return direction * (aValue - bValue);
    });
  }
  return result;
}
