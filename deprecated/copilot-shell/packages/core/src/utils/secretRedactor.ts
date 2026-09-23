/**
 * @license
 * Copyright 2026 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Secret redaction utilities for preventing sensitive information leakage.
 * Detects and masks API keys, tokens, passwords, and other credentials
 * in text output and tool results.
 */

import type { Part, PartListUnion } from '@google/genai';
import type { AnsiOutput } from './terminalSerializer.js';

/**
 * Represents a secret pattern to detect and redact.
 */
interface SecretPattern {
  /** Regex pattern with a capture group for the secret value */
  pattern: RegExp;
  /**
   * Replacement function or string.
   * If a function, receives the full match and returns the redacted version.
   */
  replacement:
    | string
    | ((substring: string, ...args: Array<string | number>) => string);
}

/**
 * Secret key field names used in JSON, YAML, and config files.
 * Note: bare "token" and "secret" are intentionally excluded to avoid
 * false positives with common programming terms.
 */
const SECRET_FIELD_NAMES =
  'api[_-]?key|secret[_-]?key|private[_-]?key|access[_-]?token|auth[_-]?token|refresh[_-]?token|password|passwd|credentials?';

/**
 * Ordered list of secret patterns to detect and redact.
 * Order matters: more specific patterns should come first to avoid
 * partial matches by more general patterns.
 */
const SECRET_PATTERNS: SecretPattern[] = [
  // 1. OpenAI-style API keys: sk-... (at least 20 chars after sk-)
  {
    pattern: /\bsk-[a-zA-Z0-9_-]{20,}\b/g,
    replacement: (match) => `sk-${'*'.repeat(Math.min(match.length - 3, 20))}`,
  },

  // 2. Bearer tokens: Bearer <token>
  {
    pattern: /\bBearer\s+[a-zA-Z0-9\-._~+/]+=*/gi,
    replacement: (match) => {
      const prefix = match.slice(0, 7); // "Bearer "
      return `${prefix}${'*'.repeat(8)}`;
    },
  },

  // 3. Aliyun AccessKeySecret in JSON context:
  //    "accessKeySecret": "value" or access_key_secret = "value"
  {
    pattern:
      /["']?(?:accessKeySecret|access_key_secret|AccessKeySecret)["']?\s*[:=]\s*["']([a-zA-Z0-9+/=]{16,})["']/g,
    replacement: (match) => {
      const sepIndex = Math.max(match.indexOf(':'), match.indexOf('='));
      if (sepIndex === -1) return match;
      return `${match.slice(0, sepIndex + 1)} "${'*'.repeat(8)}"`;
    },
  },

  // 4. Generic secret fields in JSON/config/JS format:
  //    "apiKey": "value", apiKey: "value", api_key = "value", etc.
  {
    pattern: new RegExp(
      `["']?\\b(?:${SECRET_FIELD_NAMES})\\b["']?\\s*[:=]\\s*["']([^"']{8,})["']`,
      'gi',
    ),
    replacement: (match) => {
      const sepIndex = Math.max(match.indexOf(':'), match.indexOf('='));
      if (sepIndex === -1) return match;
      return `${match.slice(0, sepIndex + 1)} "${'*'.repeat(8)}"`;
    },
  },

  // 5. Environment variable assignment with sensitive names:
  //    API_KEY=value, SECRET_KEY=value, ACCESS_KEY_ID=value, etc.
  //    Note: uses (?:^|[\s"';]|export\s+) instead of \b because \b
  //    doesn't match at underscore boundaries (e.g., ALIBABA_CLOUD_ACCESS_KEY_SECRET).
  {
    pattern:
      /(?:^|[\s"';]|export\s+)(?:[A-Z_]*?(?:API[_-]?KEY|SECRET[_-]?KEY|ACCESS[_-]?KEY(?:[_-]?ID|[_-]?SECRET)?|ACCESS[_-]?SECRET|AUTH[_-]?TOKEN|API[_-]?SECRET|PRIVATE[_-]?KEY|PASSWORD|PASSWD))[A-Z_]*\s*=\s*['"]?([^\s'"#;]{8,})['"]?/gim,
    replacement: (match) => {
      const eqIndex = match.indexOf('=');
      if (eqIndex === -1) return match;
      return `${match.slice(0, eqIndex + 1)}${'*'.repeat(8)}`;
    },
  },

  // 6. Anthropic API keys: ant-...
  {
    pattern: /\bant-[a-zA-Z0-9_-]{20,}\b/g,
    replacement: (match) => `ant-${'*'.repeat(Math.min(match.length - 4, 20))}`,
  },

  // 7. Alibaba Cloud AccessKey ID: LTAI... (16+ chars)
  {
    pattern: /\bLTAI[a-zA-Z0-9]{12,}\b/g,
    replacement: (match) => `LTAI${'*'.repeat(Math.min(match.length - 4, 20))}`,
  },
];

/**
 * Detect and redact sensitive information from a text string.
 * Replaces detected secrets with masked versions (e.g., `sk-****`).
 *
 * @param text - The input text to scan for secrets
 * @returns The text with sensitive information redacted
 */
export function redactSecrets(text: string): string {
  if (!text || typeof text !== 'string') {
    return text;
  }

  let result = text;

  for (const { pattern, replacement } of SECRET_PATTERNS) {
    // Reset lastIndex for global regexes
    pattern.lastIndex = 0;
    result = result.replace(
      pattern,
      replacement as Parameters<typeof String.prototype.replace>[1],
    );
  }

  return result;
}

/**
 * Redact secrets from a PartListUnion (which may be a string, Part, or Part[]).
 * Handles all forms of PartListUnion by extracting text, redacting, and reconstructing.
 *
 * @param content - The PartListUnion content to redact
 * @returns The redacted content
 */
export function redactPartListUnion(content: PartListUnion): PartListUnion {
  if (typeof content === 'string') {
    return redactSecrets(content);
  }

  if (Array.isArray(content)) {
    return content.map((part) => {
      if (typeof part === 'string') {
        return redactSecrets(part);
      }
      if (part && typeof part === 'object' && 'text' in part && part.text) {
        return { ...part, text: redactSecrets(part.text) } as Part;
      }
      return part;
    });
  }

  // Single Part object
  if (
    content &&
    typeof content === 'object' &&
    'text' in content &&
    content.text
  ) {
    return { ...content, text: redactSecrets(content.text) } as Part;
  }

  return content;
}

/**
 * Redact secrets from an AnsiOutput (terminal serialized output).
 * Iterates through all lines and tokens, redacting text content.
 *
 * @param output - The AnsiOutput to redact
 * @returns A new AnsiOutput with secrets redacted
 */
export function redactAnsiOutput(output: AnsiOutput): AnsiOutput {
  return output.map((line) =>
    line.map((token) => ({
      ...token,
      text: redactSecrets(token.text),
    })),
  );
}

/**
 * Check if a text string contains any detectable secrets.
 * Useful for logging/auditing without performing redaction.
 *
 * @param text - The input text to scan
 * @returns True if secrets were detected
 */
export function containsSecrets(text: string): boolean {
  if (!text || typeof text !== 'string') {
    return false;
  }

  for (const { pattern } of SECRET_PATTERNS) {
    pattern.lastIndex = 0;
    if (pattern.test(text)) {
      return true;
    }
  }

  return false;
}
