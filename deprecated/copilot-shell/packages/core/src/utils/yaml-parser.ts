/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { load } from 'js-yaml';

/**
 * Parses a YAML string into a JavaScript object.
 *
 * Uses js-yaml for full YAML 1.2 compatibility, including inline arrays
 * (`[a, b, c]`), nested objects, block scalars, quoted strings, etc.
 *
 * @param yamlString - YAML string to parse
 * @returns Parsed object
 */
export function parse(yamlString: string): Record<string, unknown> {
  const result = load(yamlString);
  if (result == null) {
    return {};
  }
  if (typeof result !== 'object' || Array.isArray(result)) {
    return {};
  }
  return result as Record<string, unknown>;
}

/**
 * Converts a JavaScript object to a simple YAML string.
 *
 * Produces a compact, human-readable YAML format suitable for frontmatter:
 * - Arrays are rendered as block sequences (  - item)
 * - Nested objects are rendered with 2-space indentation
 * - Strings containing special characters are double-quoted with escaping
 *
 * @param obj - Object to stringify
 * @returns YAML string (no trailing newline)
 */
export function stringify(
  obj: Record<string, unknown>,
  _options?: { lineWidth?: number; minContentWidth?: number },
): string {
  const lines: string[] = [];

  for (const [key, value] of Object.entries(obj)) {
    if (Array.isArray(value)) {
      lines.push(`${key}:`);
      for (const item of value) {
        lines.push(`  - ${formatValue(item)}`);
      }
    } else if (typeof value === 'object' && value !== null) {
      lines.push(`${key}:`);
      for (const [subKey, subValue] of Object.entries(
        value as Record<string, unknown>,
      )) {
        lines.push(`  ${subKey}: ${formatValue(subValue)}`);
      }
    } else {
      lines.push(`${key}: ${formatValue(value)}`);
    }
  }

  return lines.join('\n');
}

/**
 * Formats a value for YAML output.
 */
function formatValue(value: unknown): string {
  if (typeof value === 'string') {
    if (needsDoubleQuoting(value)) {
      return doubleQuote(value);
    }
    return value;
  }

  if (typeof value === 'object' && value !== null) {
    // Deeper than the single nesting level this formatter supports —
    // nested objects AND arrays at any depth. Serialize as a quoted JSON
    // string so the value stays recoverable instead of degrading to
    // "[object Object]" (nested arrays of objects) or a lossy String()
    // join (nested arrays of primitives).
    return doubleQuote(JSON.stringify(value));
  }

  return String(value);
}

/**
 * Reports whether a string value must be emitted in double-quoted style to
 * survive a stringify/parse round-trip.
 */
function needsDoubleQuoting(value: string): boolean {
  // Special characters and untrimmed whitespace always require quoting.
  if (
    value.includes(':') ||
    value.includes('#') ||
    value.includes('"') ||
    value.includes('\\') ||
    value.trim() !== value ||
    /[\n\r\t]/.test(value)
  ) {
    return true;
  }
  // Multi-line plain scalars fold newlines on re-parse, and strings that
  // re-parse as another YAML type (booleans, numbers, null, ...) or that
  // are not valid plain scalars at all must be quoted too. Round-tripping
  // the candidate through the same parser keeps this exactly consistent
  // with what parse() will do.
  try {
    return typeof load(value) !== 'string';
  } catch {
    return true;
  }
}

/**
 * Emits a string in double-quoted style with YAML escape sequences.
 * Backslashes are escaped first, then quotes, then control characters.
 */
function doubleQuote(value: string): string {
  const escaped = value
    .replace(/\\/g, '\\\\')
    .replace(/"/g, '\\"')
    .replace(/\n/g, '\\n')
    .replace(/\r/g, '\\r')
    .replace(/\t/g, '\\t');
  return `"${escaped}"`;
}
