// Display-text truncation that never splits a UTF-16 surrogate pair.
//
// `String.prototype.slice` counts UTF-16 code units, so a cut landing inside
// an astral-plane character (emoji, CJK Extension B+) leaves a lone high
// surrogate that the browser renders as U+FFFD next to the ellipsis. The
// shortId-style clips on machine identifiers (session / tool-call /
// trajectory IDs) deliberately stay on raw `slice`; content-bearing
// truncations must route through this helper.

/**
 * Clip `text` to at most `max` UTF-16 code units without splitting a
 * surrogate pair, appending an ellipsis when the text was clipped.
 */
export function truncateText(text: string, max: number): string {
  if (text.length <= max) return text;
  const cut = text.slice(0, max);
  const last = cut.charCodeAt(cut.length - 1);
  return (last >= 0xd800 && last <= 0xdbff ? cut.slice(0, -1) : cut) + '\u2026';
}
