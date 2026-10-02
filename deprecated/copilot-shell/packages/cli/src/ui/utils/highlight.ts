/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { cpLen, cpSlice } from './textUtils.js';
import { isSlashCommand } from './commandUtils.js';
import type { SlashCommand } from '../commands/types.js';

export type HighlightToken = {
  text: string;
  type: 'default' | 'command' | 'file' | 'placeholder';
};

const HIGHLIGHT_REGEX = /(^\/[a-zA-Z0-9_-]+|@(?:\\ |[a-zA-Z0-9_./-])+)/g;

/**
 * Placeholder marker: Unicode Private Use Area character \uE000
 *
 * DESIGN CHOICE: We use U+E000 (PUA-0) as a single-character marker to represent
 * large pasted content in the input buffer. This avoids multi-character placeholder
 * strings that cause cursor positioning issues.
 *
 * ASSUMPTION: U+E000 is rarely used in typical user input. While theoretically
 * users could input this character from external editors or copy-paste content
 * containing it, this is extremely unlikely in normal CLI usage.
 *
 * POTENTIAL CONFLICT: If user content contains \uE000, it will be treated as a
 * placeholder marker and trigger placeholder rendering/deletion logic. To prevent
 * this in external editor mode, the editor callback should filter out \uE000 from
 * user content before writing back to the buffer.
 *
 * Each marker represents one pasted content placeholder (atomic unit).
 */
export const PLACEHOLDER_MARKER = '\uE000';

export function parseInputForHighlighting(
  text: string,
  index: number,
  commands?: readonly SlashCommand[],
): readonly HighlightToken[] {
  if (!text) {
    return [{ text: '', type: 'default' }];
  }

  // First pass: split text by placeholder markers
  const preProcessedTokens: Array<{
    text: string;
    type: 'default' | 'placeholder';
  }> = [];
  let lastIndex = 0;

  for (let i = 0; i < text.length; i++) {
    if (text[i] === PLACEHOLDER_MARKER) {
      // Add any text before this marker as default token
      if (i > lastIndex) {
        preProcessedTokens.push({
          text: text.slice(lastIndex, i),
          type: 'default',
        });
      }
      // Add the marker as a placeholder token
      preProcessedTokens.push({
        text: PLACEHOLDER_MARKER,
        type: 'placeholder',
      });
      lastIndex = i + 1;
    }
  }

  // Add any remaining text after the last marker
  if (lastIndex < text.length) {
    preProcessedTokens.push({
      text: text.slice(lastIndex),
      type: 'default',
    });
  }

  // Second pass: process default tokens for commands and file references
  const finalTokens: HighlightToken[] = [];

  for (const token of preProcessedTokens) {
    if (token.type === 'placeholder') {
      finalTokens.push(token);
      continue;
    }

    // Process default tokens for commands and file references
    HIGHLIGHT_REGEX.lastIndex = 0;
    let match;
    let tokenLastIndex = 0;

    while ((match = HIGHLIGHT_REGEX.exec(token.text)) !== null) {
      const [fullMatch] = match;
      const matchIndex = match.index;

      // Add text before the match as default token
      if (matchIndex > tokenLastIndex) {
        finalTokens.push({
          text: token.text.slice(tokenLastIndex, matchIndex),
          type: 'default',
        });
      }

      // Add the matched token
      const type = fullMatch.startsWith('/') ? 'command' : 'file';
      if (type === 'command') {
        const stillTyping = matchIndex + fullMatch.length >= token.text.length;
        if (index !== 0 || !isSlashCommand(fullMatch, commands, stillTyping)) {
          finalTokens.push({ text: fullMatch, type: 'default' });
        } else {
          finalTokens.push({ text: fullMatch, type });
        }
      } else {
        finalTokens.push({ text: fullMatch, type });
      }

      tokenLastIndex = matchIndex + fullMatch.length;
    }

    // Add remaining text after last match
    if (tokenLastIndex < token.text.length) {
      finalTokens.push({
        text: token.text.slice(tokenLastIndex),
        type: 'default',
      });
    }
  }

  // If no tokens were created, return a single default token with the original text
  if (finalTokens.length === 0) {
    return [{ text, type: 'default' }];
  }

  return finalTokens;
}

export function buildSegmentsForVisualSlice(
  tokens: readonly HighlightToken[],
  sliceStart: number,
  sliceEnd: number,
): readonly HighlightToken[] {
  if (sliceStart >= sliceEnd) return [];

  const segments: HighlightToken[] = [];
  let tokenCpStart = 0;

  for (const token of tokens) {
    const tokenLen = cpLen(token.text);
    const tokenStart = tokenCpStart;
    const tokenEnd = tokenStart + tokenLen;

    const overlapStart = Math.max(tokenStart, sliceStart);
    const overlapEnd = Math.min(tokenEnd, sliceEnd);
    if (overlapStart < overlapEnd) {
      const sliceStartInToken = overlapStart - tokenStart;
      const sliceEndInToken = overlapEnd - tokenStart;
      const rawSlice = cpSlice(token.text, sliceStartInToken, sliceEndInToken);

      const last = segments[segments.length - 1];
      // Don't merge placeholder tokens - each placeholder must remain as a separate segment
      // for correct i18n display (merged placeholder text won't match the placeholder regex)
      if (last && last.type === token.type && token.type !== 'placeholder') {
        last.text += rawSlice;
      } else {
        segments.push({ type: token.type, text: rawSlice });
      }
    }

    tokenCpStart += tokenLen;
  }

  return segments;
}
