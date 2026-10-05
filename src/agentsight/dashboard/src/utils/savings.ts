/**
 * Compounded savings as a percentage of the baseline (original) token count.
 *
 * One definition for every surface: the summary card, the per-session rows and
 * the ATIF savings card all render "saved / baseline × 100". The server's
 * `compounded_savings_rate` divides by the *actual* token count instead, so
 * using it here made a row contradict its own tooltip and exceed 100 %.
 */
export function compoundedSavingsRate(
  compoundedSaved: number,
  baselineTokens: number,
): number {
  if (!(baselineTokens > 0)) {
    return 0;
  }
  return (compoundedSaved / baselineTokens) * 100;
}
