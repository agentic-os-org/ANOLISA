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
    // Quote strings that might be ambiguous or contain special characters
    if (
      value.includes(':') ||
      value.includes('#') ||
      value.includes('"') ||
      value.includes('\\') ||
      value.trim() !== value
    ) {
      // Escape backslashes THEN quotes
      return `"${value.replace(/\\/g, '\\\\').replace(/"/g, '\\"')}"`;
    }
    return value;
  }

  return String(value);
}
