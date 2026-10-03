// Shared locale-aware timestamp formatting helpers. All AgentSight event
// timestamps are nanoseconds since epoch; JS Date wants milliseconds.

const NS_PER_MS = 1_000_000;

/** Convert a nanosecond timestamp to the locale's default date-time string. */
export function formatNs(ns: number, locale: string): string {
  return new Date(ns / NS_PER_MS).toLocaleString(locale);
}

/** Zero-padded full date-time (`YYYY-MM-DD HH:mm:ss` style) for list views. */
export function formatNsPadded(ns: number, locale: string): string {
  return new Date(ns / NS_PER_MS).toLocaleString(locale, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  });
}

/** Compact `MM-DD HH:mm:ss` variant working on milliseconds. */
export function formatMsCompact(ms: number, locale: string): string {
  return new Intl.DateTimeFormat(locale, {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(ms);
}

/** Compact variant on nanoseconds; renders '—' for null/zero timestamps. */
export function formatNsCompact(ns: number | null, locale: string): string {
  if (!ns) return '—';
  return formatMsCompact(ns / NS_PER_MS, locale);
}
