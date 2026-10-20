/**
 * BM25 auto-recall keyword extraction.
 *
 * Long natural-language prompts dilute BM25 signal with stopwords,
 * causing FTS5 to return empty results. This module extracts salient
 * keywords and builds progressively shorter query candidates so the
 * BM25 search can find matching memories.
 */

/** English stopwords filtered before keyword extraction. */
const STOP_WORDS = new Set([
  "a", "an", "the", "is", "are", "was", "were", "be", "been", "being",
  "have", "has", "had", "do", "does", "did", "will", "would", "should",
  "could", "may", "might", "must", "shall", "can", "what", "who", "whom",
  "whose", "which", "that", "this", "these", "those", "i", "you", "he",
  "she", "it", "we", "they", "me", "him", "her", "us", "them", "my",
  "your", "his", "its", "our", "their", "to", "of", "in", "on",
  "at", "by", "for", "with", "about", "against", "between", "into",
  "through", "during", "before", "after", "above", "below", "from",
  "up", "down", "out", "off", "over", "under", "again", "further",
  "then", "once", "here", "there", "when", "where", "why", "how", "all",
  "any", "both", "each", "few", "more", "most", "other", "some", "such",
  "no", "nor", "not", "only", "own", "same", "so", "than", "too", "very",
  "just", "also", "and", "but", "or", "if", "because", "as", "until",
  "while", "answer", "tell", "give", "say", "know", "think", "want",
  "need", "like", "get", "got", "make", "made", "go", "going",
]);

/**
 * Maximum keywords retained. Stride-sampled from the full list so later
 * words in the prompt are represented, not just the first N.
 */
const MAX_KEYWORDS = 8;

/**
 * Maximum UTF-8 byte length for any single query candidate.  The
 * server-side memory_search tool rejects queries over 1024 bytes.
 */
export const MAX_QUERY_BYTES = 1024;

/**
 * UTF-16 code units of the raw prompt kept for the short raw fallback
 * candidate.  Cut on a code point boundary, never mid surrogate pair.
 */
const RAW_PROMPT_FALLBACK_UNITS = 200;

/**
 * Maximum number of results to inject into the prompt.  Aligns with
 * the per-candidate top_k so we never show more than 5 memories.
 */
export const MAX_RESULTS = 5;

/**
 * Reciprocal-rank fusion constant.  Standard RRF uses k=60.
 */
export const RRF_K = 60;

/**
 * Extract salient keywords from text.
 *
 * Strips non-ASCII characters (emoji, CJK, Cyrillic, accented letters,
 * curly quotes) before tokenising, so English prompts containing a
 * stray emoji or smart-quote still produce useful keywords.  Purely
 * non-ASCII text (e.g. a CJK-only prompt) naturally yields zero
 * keywords, which the caller treats as "no extraction possible" and
 * falls through to the raw-query path.
 *
 * Keywords are lowercased, deduplicated, and filtered to >= 2 chars.
 * While 2-char tokens (AI, ID, UK) cause the BM25 backend to fall back
 * to LIKE search with AND semantics, they are important domain terms
 * that should still be included — other candidates (CJK trigrams,
 * raw query) will also be tried and merged via reciprocal-rank fusion.
 *
 * When the unique keyword count exceeds MAX_KEYWORDS, endpoints-
 * inclusive linear spacing ensures both the first and last keyword
 * are always included.
 */
export function extractKeywords(text: string): string {
  const asciiOnly = text.replace(/[^\x00-\x7F]/g, " ");

  const words = asciiOnly
    .toLowerCase()
    .replace(/[^a-z0-9\s]/g, " ")
    .split(/\s+/)
    .filter((w) => w.length >= 2 && !STOP_WORDS.has(w));

  const seen = new Set<string>();
  const unique = words.filter((w) => {
    if (seen.has(w)) return false;
    seen.add(w);
    return true;
  });

  if (unique.length <= MAX_KEYWORDS) return unique.join(" ");

  // Endpoints-inclusive linear spacing: index 0 is always the first
  // keyword and index (length-1) is always the last.
  return Array.from({ length: MAX_KEYWORDS }, (_, i) =>
    unique[Math.round((i * (unique.length - 1)) / (MAX_KEYWORDS - 1))]
  ).join(" ");
}

/**
 * Extract CJK trigram segments from text for query candidates.
 *
 * CJK text lacks word boundaries (spaces), so a long CJK sentence
 * cannot be meaningfully split into "keywords".  Instead, we extract
 * overlapping trigrams (3 consecutive CJK characters) from the text.
 * Each trigram is >= 3 characters, so the BM25 backend's trigram
 * tokenizer can match them directly without falling into the
 * `search_like()` AND path.
 *
 * Returns an array of trigram strings.  The caller joins them into
 * space-separated query candidates.
 */
export function extractCjkTrigrams(text: string): string[] {
  // Keep only CJK Unified Ideographs (U+4E00..U+9FFF), Hiragana
  // (U+3040..U+309F), Katakana (U+30A0..U+30FF), and CJK
  // Compatibility (U+F900..U+FAFF).
  const cjkChars = text.replace(/[^\u4e00-\u9fff\u3040-\u309f\u30a0-\u30ff\uf900-\ufaff]/g, "");
  if (cjkChars.length < 3) return [];

  // Extract overlapping trigrams from the CJK character sequence.
  const trigrams: string[] = [];
  const seen = new Set<string>();
  for (let i = 0; i < cjkChars.length - 2; i++) {
    const trigram = cjkChars[i] + cjkChars[i + 1] + cjkChars[i + 2];
    if (!seen.has(trigram)) {
      seen.add(trigram);
      trigrams.push(trigram);
    }
  }
  return trigrams;
}

/**
 * Snap an exclusive UTF-16 cut point back to a code point boundary.
 *
 * `text.slice(0, index)` must never end on the leading half of a
 * surrogate pair.  An unpaired surrogate is not a valid prefix of the
 * source: TextEncoder scores it as U+FFFD (3 bytes) instead of the
 * 4-byte code point it belongs to, and JSON.stringify escapes it as a
 * lone `\udXXX`, so the Rust MCP server would receive malformed UTF-8
 * rather than the query we meant to send.
 *
 * Only a cut that actually splits a pair moves; text that is already
 * unpaired upstream is left untouched, keeping ASCII and BMP behaviour
 * byte-for-byte identical.
 */
function toCodePointBoundary(text: string, index: number): number {
  if (index <= 0 || index >= text.length) return index;
  const prev = text.charCodeAt(index - 1);
  if (prev < 0xd800 || prev > 0xdbff) return index;
  const next = text.charCodeAt(index);
  if (next < 0xdc00 || next > 0xdfff) return index;
  return index - 1;
}

/**
 * Keep at most `maxUnits` UTF-16 code units of `text`, dropping a
 * surrogate pair the cut would otherwise split in half.
 */
function sliceCodeUnits(text: string, maxUnits: number): string {
  return text.slice(0, toCodePointBoundary(text, maxUnits));
}

/**
 * Truncate a string so its UTF-8 encoding does not exceed maxBytes,
 * cutting only on code point boundaries.
 */
function truncateToByteLimit(text: string, maxBytes: number): string {
  if (new TextEncoder().encode(text).byteLength <= maxBytes) return text;

  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = Math.ceil((lo + hi) / 2);
    if (new TextEncoder().encode(text.slice(0, mid)).byteLength <= maxBytes) {
      lo = mid;
    } else {
      hi = mid - 1;
    }
  }
  // `lo` is the widest cut whose *encoding* fits, but TextEncoder bills
  // an unpaired high surrogate 3 bytes (U+FFFD) — one less than the
  // 4-byte code point it is half of.  So the widest fitting cut can be a
  // split pair whose real encoding would overflow the budget.  Backing
  // off to the boundary yields the longest genuinely valid prefix.
  return text.slice(0, toCodePointBoundary(text, lo));
}

/**
 * Build deduplicated query candidates for BM25 auto-recall.
 *
 * Candidates:
 * 1. Extracted keywords (stride-sampled with tail coverage, <= 8 words).
 * 2. First 3 keywords (shorter fallback for sparse corpora).
 * 3. CJK trigram segments (sampled, <= 8 trigrams) — for CJK prompts
 *    that have no space-separated keywords.  Each trigram is >= 3
 *    chars so the BM25 backend uses trigram matching, not LIKE AND.
 * 4. CJK trigram top-3 — shorter CJK fallback.
 * 5. Truncated raw prompt (first 200 UTF-16 units, byte-safe).
 * 6. Full raw prompt (byte-safe, <= 1024 bytes) — last resort.
 *
 * All candidates are truncated to MAX_QUERY_BYTES (1024) UTF-8 bytes.
 * Every cut lands on a code point boundary, so astral characters (emoji,
 * CJK extension B) are kept whole or dropped — never halved into an
 * unpaired surrogate.  The list is deduplicated (order-preserving).
 */
export function buildRecallQueries(text: string): string[] {
  const keywords = extractKeywords(text);
  const cjkTrigrams = extractCjkTrigrams(text);

  const truncated = truncateToByteLimit(
    sliceCodeUnits(text, RAW_PROMPT_FALLBACK_UNITS),
    MAX_QUERY_BYTES,
  );
  const fullText = truncateToByteLimit(text, MAX_QUERY_BYTES);

  const candidates: string[] = [];
  if (keywords) candidates.push(keywords);

  const top3 = keywords ? keywords.split(" ").slice(0, 3).join(" ") : "";
  if (top3) candidates.push(top3);

  // CJK trigram candidates: stride-sample up to MAX_KEYWORDS trigrams,
  // joined with spaces so each trigram (>= 3 chars) is matched by the
  // BM25 backend's trigram tokenizer.
  if (cjkTrigrams.length > 0) {
    let trigramQuery: string;
    if (cjkTrigrams.length <= MAX_KEYWORDS) {
      trigramQuery = cjkTrigrams.join(" ");
    } else {
      const step = (cjkTrigrams.length - 1) / (MAX_KEYWORDS - 1);
      trigramQuery = Array.from({ length: MAX_KEYWORDS }, (_, i) =>
        cjkTrigrams[Math.round(i * step)]
      ).join(" ");
    }
    candidates.push(trigramQuery);

    // Shorter CJK fallback: first 3 trigrams.
    const trigramTop3 = cjkTrigrams.slice(0, 3).join(" ");
    if (trigramTop3 !== trigramQuery) candidates.push(trigramTop3);
  }

  if (truncated.length >= 2) candidates.push(truncated);
  if (fullText.length >= 2 && fullText !== truncated) candidates.push(fullText);

  const seen = new Set<string>();
  return candidates.filter((q) => {
    if (q.length < 2 || seen.has(q)) return false;
    if (new TextEncoder().encode(q).byteLength > MAX_QUERY_BYTES) return false;
    seen.add(q);
    return true;
  });
}
