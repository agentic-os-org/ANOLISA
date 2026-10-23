/** A host can inspect these fields without matching a diagnostic string. */
export class TokenBudgetExceededError extends RangeError {
  constructor(
    public readonly requiredTokens: number,
    public readonly tokenBudget: number,
    public readonly freshTailTurnCount: number,
    public readonly freshTailMessageCount: number,
  ) {
    super(
      Number.isFinite(tokenBudget) && tokenBudget > 0
        ? `Fresh-tail context (${requiredTokens} tokens) exceeds token budget (${tokenBudget}); reduce the recent input or raise the host budget`
        : `Invalid token budget (${tokenBudget}); expected a finite positive number`,
    );
    this.name = "TokenBudgetExceededError";
  }
}
