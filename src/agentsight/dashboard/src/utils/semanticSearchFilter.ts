/**
 * Pure merge logic for the semantic session search: apply LLM-ranked results
 * on top of the candidate list. Kept outside the page component so the
 * loading/empty behaviors are unit-testable without a browser.
 */

export interface RankableSession {
  session_id: string;
}

export interface RelevanceEntry {
  relevance: string;
}

export function applySemanticRanking<T extends RankableSession>(
  base: T[],
  search: string,
  semanticMatches: Record<string, RelevanceEntry>,
  semanticLoading: boolean,
): T[] {
  const q = search.trim();
  if (!q) return base;

  const ranked: T[] = [];
  const entries = Object.entries(semanticMatches).sort(
    ([, a], [, b]) => (a.relevance === 'high' ? 0 : 1) - (b.relevance === 'high' ? 0 : 1),
  );
  for (const [id] of entries) {
    const s = base.find((x) => x.session_id === id);
    if (s) ranked.push(s);
  }

  // While the LLM ranks candidates, keep showing the list so the UI never
  // flashes "no matching sessions" for up to the LLM timeout.
  if (ranked.length === 0 && semanticLoading) return base;
  // Too few candidates to justify an LLM call — show them all instead.
  if (ranked.length === 0 && base.length <= 5) return base;
  return ranked;
}
