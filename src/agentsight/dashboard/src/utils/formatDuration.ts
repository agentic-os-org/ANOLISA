/**
 * Format a duration in seconds for compact display.
 *
 * Rounding happens before the minute split: the previous implementation
 * rounded `s % 60` independently, so 119.6 s rendered as "1m 60s",
 * 3599.7 s as "59m 60s", and 59.96 s as "60.0s" — none of which is a valid
 * duration. Values below 60 s keep one decimal.
 */
export function formatDurationSecs(seconds: number): string {
  const rounded = Math.round(seconds);
  if (rounded >= 60) {
    const minutes = Math.floor(rounded / 60);
    return `${minutes}m ${rounded % 60}s`;
  }
  return `${seconds.toFixed(1)}s`;
}
