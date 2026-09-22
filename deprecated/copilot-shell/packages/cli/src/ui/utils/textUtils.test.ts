/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect } from 'vitest';
import type {
  ToolCallConfirmationDetails,
  ToolEditConfirmationDetails,
} from '@copilot-shell/core';
import {
  escapeAnsiCtrlCodes,
  cpLen,
  cpSlice,
  toCodePoints,
} from './textUtils.js';

describe('textUtils', () => {
  describe('escapeAnsiCtrlCodes', () => {
    describe('escapeAnsiCtrlCodes string case study', () => {
      it('should replace ANSI escape codes with a visible representation', () => {
        const text = '\u001b[31mHello\u001b[0m';
        const expected = '\\u001b[31mHello\\u001b[0m';
        expect(escapeAnsiCtrlCodes(text)).toBe(expected);

        const text2 = "sh -e 'good && bad# \u001b[9D\u001b[K && good";
        const expected2 = "sh -e 'good && bad# \\u001b[9D\\u001b[K && good";
        expect(escapeAnsiCtrlCodes(text2)).toBe(expected2);
      });

      it('should not change a string with no ANSI codes', () => {
        const text = 'Hello, world!';
        expect(escapeAnsiCtrlCodes(text)).toBe(text);
      });

      it('should handle an empty string', () => {
        expect(escapeAnsiCtrlCodes('')).toBe('');
      });

      describe('toolConfirmationDetails case study', () => {
        it('should sanitize command and rootCommand for exec type', () => {
          const details: ToolCallConfirmationDetails = {
            title: '\u001b[34mfake-title\u001b[0m',
            type: 'exec',
            command: '\u001b[31mmls -l\u001b[0m',
            rootCommand: '\u001b[32msudo apt-get update\u001b[0m',
            onConfirm: async () => {},
          };

          const sanitized = escapeAnsiCtrlCodes(details);

          if (sanitized.type === 'exec') {
            expect(sanitized.title).toBe('\\u001b[34mfake-title\\u001b[0m');
            expect(sanitized.command).toBe('\\u001b[31mmls -l\\u001b[0m');
            expect(sanitized.rootCommand).toBe(
              '\\u001b[32msudo apt-get update\\u001b[0m',
            );
          }
        });

        it('should sanitize properties for edit type', () => {
          const details: ToolCallConfirmationDetails = {
            type: 'edit',
            title: '\u001b[34mEdit File\u001b[0m',
            fileName: '\u001b[31mfile.txt\u001b[0m',
            filePath: '/path/to/\u001b[32mfile.txt\u001b[0m',
            fileDiff:
              'diff --git a/file.txt b/file.txt\n--- a/\u001b[33mfile.txt\u001b[0m\n+++ b/file.txt',
            onConfirm: async () => {},
          } as unknown as ToolEditConfirmationDetails;

          const sanitized = escapeAnsiCtrlCodes(details);

          if (sanitized.type === 'edit') {
            expect(sanitized.title).toBe('\\u001b[34mEdit File\\u001b[0m');
            expect(sanitized.fileName).toBe('\\u001b[31mfile.txt\\u001b[0m');
            expect(sanitized.filePath).toBe(
              '/path/to/\\u001b[32mfile.txt\\u001b[0m',
            );
            expect(sanitized.fileDiff).toBe(
              'diff --git a/file.txt b/file.txt\n--- a/\\u001b[33mfile.txt\\u001b[0m\n+++ b/file.txt',
            );
          }
        });

        it('should sanitize properties for mcp type', () => {
          const details: ToolCallConfirmationDetails = {
            type: 'mcp',
            title: '\u001b[34mCloud Run\u001b[0m',
            serverName: '\u001b[31mmy-server\u001b[0m',
            toolName: '\u001b[32mdeploy\u001b[0m',
            toolDisplayName: '\u001b[33mDeploy Service\u001b[0m',
            onConfirm: async () => {},
          };

          const sanitized = escapeAnsiCtrlCodes(details);

          if (sanitized.type === 'mcp') {
            expect(sanitized.title).toBe('\\u001b[34mCloud Run\\u001b[0m');
            expect(sanitized.serverName).toBe('\\u001b[31mmy-server\\u001b[0m');
            expect(sanitized.toolName).toBe('\\u001b[32mdeploy\\u001b[0m');
            expect(sanitized.toolDisplayName).toBe(
              '\\u001b[33mDeploy Service\\u001b[0m',
            );
          }
        });

        it('should sanitize properties for info type', () => {
          const details: ToolCallConfirmationDetails = {
            type: 'info',
            title: '\u001b[34mWeb Search\u001b[0m',
            prompt: '\u001b[31mSearch for cats\u001b[0m',
            urls: ['https://\u001b[32mgoogle.com\u001b[0m'],
            onConfirm: async () => {},
          };

          const sanitized = escapeAnsiCtrlCodes(details);

          if (sanitized.type === 'info') {
            expect(sanitized.title).toBe('\\u001b[34mWeb Search\\u001b[0m');
            expect(sanitized.prompt).toBe(
              '\\u001b[31mSearch for cats\\u001b[0m',
            );
            expect(sanitized.urls?.[0]).toBe(
              'https://\\u001b[32mgoogle.com\\u001b[0m',
            );
          }
        });
      });

      it('should not change the object if no sanitization is needed', () => {
        const details: ToolCallConfirmationDetails = {
          type: 'info',
          title: 'Web Search',
          prompt: 'Search for cats',
          urls: ['https://google.com'],
          onConfirm: async () => {},
        };

        const sanitized = escapeAnsiCtrlCodes(details);
        expect(sanitized).toBe(details);
      });

      it('should handle nested objects and arrays', () => {
        const details = {
          a: '\u001b[31mred\u001b[0m',
          b: {
            c: '\u001b[32mgreen\u001b[0m',
            d: ['\u001b[33myellow\u001b[0m', { e: '\u001b[34mblue\u001b[0m' }],
          },
          f: 123,
          g: null,
          h: () => '\u001b[35mpurple\u001b[0m',
        };

        const sanitized = escapeAnsiCtrlCodes(details);

        expect(sanitized.a).toBe('\\u001b[31mred\\u001b[0m');
        if (typeof sanitized.b === 'object' && sanitized.b !== null) {
          const b = sanitized.b as { c: string; d: Array<string | object> };
          expect(b.c).toBe('\\u001b[32mgreen\\u001b[0m');
          expect(b.d[0]).toBe('\\u001b[33myellow\\u001b[0m');
          if (typeof b.d[1] === 'object' && b.d[1] !== null) {
            const e = b.d[1] as { e: string };
            expect(e.e).toBe('\\u001b[34mblue\\u001b[0m');
          }
        }
        expect(sanitized.f).toBe(123);
        expect(sanitized.g).toBe(null);
        expect(sanitized.h()).toBe('\u001b[35mpurple\u001b[0m');
      });
    });
  });

  describe('code-point utilities (cpLen, cpSlice, toCodePoints)', () => {
    describe('cpLen', () => {
      it('should return correct length for ASCII strings', () => {
        expect(cpLen('hello')).toBe(5);
        expect(cpLen('')).toBe(0);
      });

      it('should return correct length for emoji/non-BMP characters', () => {
        // Single emoji (surrogate pair in UTF-16)
        expect(cpLen('🚀')).toBe(1);
        expect(cpLen('😀')).toBe(1);
        expect(cpLen('🎉')).toBe(1);

        // Multiple emojis
        expect(cpLen('🚀😀')).toBe(2);
        expect(cpLen('🎉🎉🎉')).toBe(3);

        // Mixed ASCII and emoji
        expect(cpLen('hi🚀')).toBe(3);
        expect(cpLen('🚀hello')).toBe(6);

        // Chinese characters (BMP but still single code point each)
        expect(cpLen('你好')).toBe(2);
        expect(cpLen('你好世界')).toBe(4);

        // Mixed emoji, ASCII, and CJK
        expect(cpLen('🚀hello世界')).toBe(8);
      });

      it('should handle complex emoji (surrogate pairs)', () => {
        // Some emojis are single code points (most common)
        expect(cpLen('❤️')).toBe(2); // Heart + variant selector
        expect(cpLen('👨‍👩‍👧‍👦')).toBe(7); // Family emoji (ZWJ sequence)
      });
    });

    describe('cpSlice', () => {
      it('should slice ASCII strings correctly', () => {
        expect(cpSlice('hello', 0, 3)).toBe('hel');
        expect(cpSlice('hello', 2)).toBe('llo');
        expect(cpSlice('hello', 1, 4)).toBe('ell');
      });

      it('should slice emoji/non-BMP strings correctly', () => {
        // Single emoji
        expect(cpSlice('🚀', 0, 1)).toBe('🚀');
        expect(cpSlice('🚀', 0)).toBe('🚀');

        // Multiple emojis
        expect(cpSlice('🚀😀🎉', 0, 2)).toBe('🚀😀');
        expect(cpSlice('🚀😀🎉', 1)).toBe('😀🎉');
        expect(cpSlice('🚀😀🎉', 1, 3)).toBe('😀🎉');

        // Mixed ASCII and emoji
        expect(cpSlice('hi🚀world', 0, 3)).toBe('hi🚀');
        expect(cpSlice('hi🚀world', 2, 5)).toBe('🚀wo');
        expect(cpSlice('🚀hello', 1, 4)).toBe('hel');
      });

      it('should handle edge cases with emoji', () => {
        // Empty slice
        expect(cpSlice('🚀😀', 0, 0)).toBe('');

        // Slice at emoji boundary (critical for placeholder backspace fix)
        const placeholder = '[📎 paste 100 chars]'; // Contains 📎 emoji
        const textWithEmoji = '🚀' + placeholder;
        const placeholderStart = cpLen('🚀'); // = 1 (code-point offset)
        const placeholderEnd = cpLen(textWithEmoji); // = 1 + 19 = 20

        expect(cpSlice(textWithEmoji, placeholderStart, placeholderEnd)).toBe(
          placeholder,
        );

        // Document: UTF-16 indices differ from code-point indices for emoji
        // 🚀 UTF-16 = 2, code-point = 1
        // If we used .length for offset calculation, the slice would fail
        // The fix uses cpLen/cpSlice for consistent code-point semantics
      });
    });

    describe('toCodePoints', () => {
      it('should split ASCII into individual characters', () => {
        expect(toCodePoints('abc')).toEqual(['a', 'b', 'c']);
      });

      it('should split emoji into individual code points', () => {
        expect(toCodePoints('🚀')).toEqual(['🚀']);
        expect(toCodePoints('🚀😀')).toEqual(['🚀', '😀']);
      });

      it('should handle mixed content', () => {
        expect(toCodePoints('a🚀b')).toEqual(['a', '🚀', 'b']);
        expect(toCodePoints('hi🎉世界')).toEqual(['h', 'i', '🎉', '世', '界']);
      });
    });
  });
});
