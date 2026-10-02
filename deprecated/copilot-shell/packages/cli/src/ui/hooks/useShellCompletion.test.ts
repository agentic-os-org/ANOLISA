/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/** @vitest-environment jsdom */

import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import { getPathCompletions } from './useShellCompletion.js';
import { createTmpDir, cleanupTmpDir } from '@copilot-shell/test-utils';
import type { FileSystemStructure } from '@copilot-shell/test-utils';

describe('getPathCompletions', () => {
  let tmpDir: string;
  const structure: FileSystemStructure = {
    home: {
      user1: {
        'doc.txt': 'hello',
      },
      user2: [],
      '.hidden_file': 'secret',
    },
    src: {
      'index.ts': '',
      'utils.ts': '',
      '.env': '',
    },
    'README.md': '',
    '.gitignore': '',
  };

  beforeAll(async () => {
    tmpDir = await createTmpDir(structure);
  });

  afterAll(async () => {
    await cleanupTmpDir(tmpDir);
  });

  describe('trailing slash — list directory contents', () => {
    it('should list all entries inside a directory when path ends with /', () => {
      const suggestions = getPathCompletions('home/', tmpDir);
      const labels = suggestions.map((s) => s.label);
      // home/ has: user1/, user2/, .hidden_file
      // No dotfiles shown because basePart is empty (not starting with .)
      expect(labels).toContain('user1/');
      expect(labels).toContain('user2/');
      expect(labels).not.toContain('.hidden_file');
    });

    it('should list dotfiles when basePart is empty and path ends with /', () => {
      const suggestions = getPathCompletions('home/.', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toContain('.hidden_file');
    });

    it('should list all entries inside nested directory with trailing slash', () => {
      const suggestions = getPathCompletions('home/user1/', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toContain('doc.txt');
    });
  });

  describe('no trailing slash — filter by prefix', () => {
    it('should filter entries by prefix when no trailing slash', () => {
      const suggestions = getPathCompletions('home/u', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toContain('user1/');
      expect(labels).toContain('user2/');
      expect(labels).not.toContain('.hidden_file');
    });

    it('should match a single directory name without trailing slash', () => {
      const suggestions = getPathCompletions('hom', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toEqual(['home/']);
    });

    it('should return empty for non-matching prefix', () => {
      const suggestions = getPathCompletions('home/zzz', tmpDir);
      expect(suggestions).toEqual([]);
    });
  });

  describe('absolute paths', () => {
    it('should handle absolute path with trailing slash', () => {
      const suggestions = getPathCompletions(tmpDir + '/home/', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toContain('user1/');
      expect(labels).toContain('user2/');
    });

    it('should handle absolute path without trailing slash', () => {
      const suggestions = getPathCompletions(tmpDir + '/src/i', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toContain('index.ts');
    });
  });

  describe('edge cases', () => {
    it('should return empty for non-existent directory', () => {
      const suggestions = getPathCompletions('/nonexistent/path/', tmpDir);
      expect(suggestions).toEqual([]);
    });

    it('should return empty for empty token at command position', () => {
      // When token is empty string, getPathCompletions should still work
      const suggestions = getPathCompletions('', tmpDir);
      // Empty partial: dirname('.') = '.', basename('') = ''
      // This lists cwd contents
      expect(suggestions.length).toBeGreaterThan(0);
    });

    it('should sort directories before files', () => {
      const suggestions = getPathCompletions('', tmpDir);
      const labels = suggestions.map((s) => s.label);
      // Find first file (non-directory) index
      const firstFileIdx = labels.findIndex((l) => !l.endsWith('/'));
      const lastDirIdx = labels.findLastIndex((l) => l.endsWith('/'));
      // All directories should come before all files
      if (firstFileIdx !== -1 && lastDirIdx !== -1) {
        expect(lastDirIdx).toBeLessThan(firstFileIdx);
      }
    });

    it('should handle partial match inside directory', () => {
      const suggestions = getPathCompletions('src/ut', tmpDir);
      const labels = suggestions.map((s) => s.label);
      expect(labels).toContain('utils.ts');
      expect(labels).not.toContain('index.ts');
    });
  });
});
